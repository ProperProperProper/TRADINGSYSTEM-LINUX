"""Unit tests for candidate algorithmic trading detection and .safetensors adapter persistence."""
from __future__ import annotations

import json
import struct
import tempfile
import unittest
from pathlib import Path

from papercompmoomoo import algo_fingerprint


class AlgoFingerprintTest(unittest.TestCase):
    def test_identifies_twap_vwap_and_ignition_archetypes(self):
        base_ts = 1_793_716_200_000
        # 1. Steady upward drift with uniform volume -> TWAP / VWAP accumulation
        twap_bars = [
            (base_ts + i * 60_000, 100.0 + i * 0.08, 100.10 + i * 0.08, 99.98 + i * 0.08, 100.08 + i * 0.08, 5000.0)
            for i in range(15)
        ]
        fp_twap = algo_fingerprint.analyze_bars("US.STX", twap_bars)
        self.assertEqual(fp_twap["algo_direction"], "bullish_accumulation")
        self.assertEqual(fp_twap["counter_strategy"]["decision"], "enter")
        self.assertEqual(len(fp_twap["feature_vector"]), 12)

        # 2. Explosive volume surge and range expansion -> MOMENTUM_IGNITION_SWEEP
        ignition_bars = [
            (base_ts + i * 60_000, 50.0, 50.05, 49.95, 50.02, 1000.0)
            for i in range(12)
        ] + [
            (base_ts + (12 + j) * 60_000, 50.2 + j * 0.6, 50.9 + j * 0.6, 50.1 + j * 0.6, 50.85 + j * 0.6, 15_000.0)
            for j in range(3)
        ]
        fp_ign = algo_fingerprint.analyze_bars("US.LSCC", ignition_bars)
        self.assertEqual(fp_ign["algo_type"], "MOMENTUM_IGNITION_SWEEP")
        self.assertEqual(fp_ign["counter_strategy"]["entry_mode"], "ride_ignition_breakout")

        # 3. Moomoo OpenD telemetry (tick tape, L2 book, capital flow distribution) fusion
        tele = {
            "exchange_vwap": 100.55,
            "subpenny_darkpool_ratio": 0.42,
            "odd_lot_ratio": 0.78,
            "buy_sweep_ratio": 0.68,
            "sell_sweep_ratio": 0.14,
            "order_book_imbalance": 0.62,
            "bid_wall_price": 100.40,
            "bid_wall_vol": 18500.0,
            "net_whale_flow_usd": 2_400_000.0,
            "net_slicer_flow_usd": 5_100_000.0,
            "slicer_participation_share": 0.64,
            "dominant_lot_size": 50,
            "median_inter_trade_ms": 220.0,
            "burst_cluster_ratio": 0.35,
        }
        fp_moomoo = algo_fingerprint.analyze_bars("US.SMCI", twap_bars, moomoo_telemetry=tele)
        self.assertIn("Moomoo:", fp_moomoo["equation"])
        self.assertEqual(fp_moomoo["parameters"]["odd_lot_ratio"], 0.78)
        self.assertEqual(fp_moomoo["parameters"]["subpenny_darkpool_ratio"], 0.42)
        self.assertEqual(fp_moomoo["parameters"]["net_slicer_flow_usd"], 5_100_000.0)

    def test_writes_spec_compliant_safetensors_adapter(self):
        with tempfile.TemporaryDirectory() as tmp:
            adapter_dir = Path(tmp) / "adapter"
            base_ts = 1_793_716_200_000
            bars = [
                (base_ts + i * 60_000, 20.0 + i * 0.05, 20.1 + i * 0.05, 19.95 + i * 0.05, 20.05 + i * 0.05, 4000.0)
                for i in range(10)
            ]
            fp = algo_fingerprint.analyze_bars("US.NXT", bars)
            st_path = algo_fingerprint.write_safetensors_adapter(adapter_dir, fingerprints=[fp])
            self.assertTrue(st_path.exists())
            self.assertGreater(st_path.stat().st_size, 100)
            raw = st_path.read_bytes()
            header_len = struct.unpack("<Q", raw[:8])[0]
            header = json.loads(raw[8 : 8 + header_len].decode("utf-8"))
            self.assertFalse(any(name.startswith('model.layers.') for name in header))
            self.assertEqual(header['__metadata__']['format'], 'classifier_only')
            self.assertIn("algo_adapter.classifier.weight", header)
            sig_path = adapter_dir / "algo_signatures.json"
            self.assertTrue(sig_path.exists())
            sigs = json.loads(sig_path.read_text())
            self.assertEqual(sigs["captured_algorithms"][0]["symbol"], "US.NXT")

    def test_open_session_qos_prioritizes_live_trading(self):
        from datetime import datetime
        from zoneinfo import ZoneInfo
        from papercompmoomoo import capacity, darwin_qos

        ny = ZoneInfo("America/New_York")
        # Tuesday 10:15 AM ET -> open session
        open_dt = datetime(2026, 11, 3, 10, 15, tzinfo=ny)
        # Tuesday 18:15 PM ET -> closed session
        closed_dt = datetime(2026, 11, 3, 18, 15, tzinfo=ny)
        self.assertTrue(darwin_qos.in_open_session(open_dt))
        self.assertFalse(darwin_qos.in_open_session(closed_dt))

        # Apply interactive and background QoS without error on Linux/macOS
        darwin_qos.set_interactive()
        self.assertEqual(darwin_qos.CURRENT_QOS, "interactive")
        darwin_qos.set_background()
        self.assertEqual(darwin_qos.CURRENT_QOS, "background")

        # Capacity throttles background workers during open sessions
        cfg = {"max_workers": 4, "pct_per_worker": 4.0}
        orig_mem, orig_cpu = capacity.memory_free_pct, capacity.cpu_idle_pct
        try:
            capacity.memory_free_pct = lambda runner=None: 28.0
            capacity.cpu_idle_pct = lambda runner=None: 40.0
            # Outside open session: 28% free RAM and 40% idle CPU allows 2 workers
            self.assertEqual(capacity.now_allowed(cfg, open_session=False), 2)
            # During open session: <30% free RAM or <45% idle CPU pauses background workers (0)
            self.assertEqual(capacity.now_allowed(cfg, open_session=True), 0)

            # Even with abundant RAM/CPU during open session, background workers are capped at 1
            capacity.memory_free_pct = lambda runner=None: 65.0
            capacity.cpu_idle_pct = lambda runner=None: 80.0
            self.assertEqual(capacity.now_allowed(cfg, open_session=False), 4)
            self.assertEqual(capacity.now_allowed(cfg, open_session=True), 1)
        finally:
            capacity.memory_free_pct = orig_mem
            capacity.cpu_idle_pct = orig_cpu

    def test_dynamic_exit_ratchets_stop_and_expands_target(self):
        base_ts = 1_793_716_200_000
        # Bullish runner climbing from 100.0 to 103.5
        bars = [
            (base_ts + i * 60_000, 100.0 + i * 0.25, 100.3 + i * 0.25, 99.95 + i * 0.25, 100.25 + i * 0.25, 8000.0)
            for i in range(15)
        ]
        res = algo_fingerprint.evaluate_dynamic_exit(
            symbol="US.STRL",
            entry_price=100.0,
            high_price=103.8,
            last_price=103.6,
            current_stop=98.5,
            current_target=102.25,
            bars=bars,
        )
        # Stop must ratchet above break-even (100.0), and target must expand above the old static 102.25
        self.assertGreater(res["dynamic_stop"], 100.0)
        self.assertGreater(res["dynamic_target"], 103.6)
        self.assertFalse(res["should_exit"])


if __name__ == "__main__":
    unittest.main()
