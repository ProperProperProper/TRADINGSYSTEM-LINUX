"""
Cross-platform Quality of Service (QoS) and Live-Session Priority Enforcement.

During open market sessions (`pre` and `open`), live trading functions and features
(`bot.py`, `orb_paper.py`, and their candidate-specific Moomoo requests) receive
top CPU and disk I/O scheduling priority (`set_interactive()`), while research,
backtesting, and administrative agents (`standby.py`, `research.py`, replay and
backtest tools) are demoted to idle/batch CPU and idle I/O scheduling
(`set_background()`) so they do not contend with live execution. Qwen learning
automation has been removed.
"""
from __future__ import annotations

import ctypes
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import platform

QOS_CLASS_USER_INTERACTIVE = 0x21  # Performance cores (P-cores) / Real-time trading
QOS_CLASS_USER_INITIATED   = 0x19  # High priority
QOS_CLASS_DEFAULT          = 0x15  # Normal system priority
QOS_CLASS_UTILITY          = 0x11  # Efficiency cores (E-cores)
QOS_CLASS_BACKGROUND       = 0x09  # Idle / background E-cores

# Linux ioprio constants (x86_64 SYS_ioprio_set = 251, aarch64 = 30)
_SYS_IOPRIO_SET = 251 if platform.machine() in ("x86_64", "amd64") else (30 if platform.machine() in ("aarch64", "arm64") else None)
_IOPRIO_WHO_PROCESS = 1
_IOPRIO_CLASS_BE = 2
_IOPRIO_CLASS_IDLE = 3
CURRENT_QOS = "default"

_lib_system = None
_lib_c = None
if platform.system() == "Darwin":
    try:
        _lib_system = ctypes.CDLL("/usr/lib/libSystem.B.dylib")
        _lib_system.pthread_set_qos_class_self_np.argtypes = [ctypes.c_uint, ctypes.c_int]
        _lib_system.pthread_set_qos_class_self_np.restype = ctypes.c_int
    except Exception:
        _lib_system = None
elif platform.system() == "Linux":
    try:
        _lib_c = ctypes.CDLL("libc.so.6")
        _lib_c.setpriority.argtypes = [ctypes.c_int, ctypes.c_uint, ctypes.c_int]
        _lib_c.setpriority.restype = ctypes.c_int
    except Exception:
        _lib_c = None


def _ioprio_value(ioclass: int, data: int) -> int:
    return (ioclass << 13) | (data & 0x1FFF)


def _linux_set_ioprio(ioclass: int, level: int = 0) -> bool:
    if _lib_c is not None and _SYS_IOPRIO_SET is not None:
        try:
            return _lib_c.syscall(ctypes.c_long(_SYS_IOPRIO_SET), _IOPRIO_WHO_PROCESS, 0, _ioprio_value(ioclass, level)) == 0
        except Exception:
            pass
    return False


def _linux_set_nice(niceness: int) -> bool:
    if _lib_c is not None:
        try:
            if _lib_c.setpriority(0, 0, int(niceness)) == 0:
                return True
            if niceness < 0:
                return _lib_c.setpriority(0, 0, 0) == 0
        except Exception:
            pass
    return False


def in_open_session(now: datetime | None = None) -> bool:
    """Return True when the market is in the live trading window (`pre` or `open`)."""
    try:
        from . import planner
        if now is not None:
            return bool(planner.live_trading_window(now))
        if planner.live_trading_window(datetime.now(timezone.utc)):
            return True
    except Exception:
        if now is not None:
            return False
    try:
        root = Path(__file__).resolve().parents[2]
        status_path = root / "data" / "status.json"
        if status_path.exists():
            phase = str(json.loads(status_path.read_text()).get("phase", "")).lower()
            if phase in ("pre", "open"):
                return True
    except Exception:
        pass
    return False


def set_interactive() -> bool:
    """Prioritize calling process/thread for live trading execution and real-time market feeds."""
    global CURRENT_QOS
    CURRENT_QOS = "interactive"
    if _lib_system:
        try:
            return _lib_system.pthread_set_qos_class_self_np(QOS_CLASS_USER_INTERACTIVE, 0) == 0
        except Exception:
            pass
    if _lib_c:
        try:
            if hasattr(os, "SCHED_OTHER"):
                os.sched_setscheduler(0, os.SCHED_OTHER, os.sched_param(0))
        except Exception:
            pass
        _linux_set_ioprio(_IOPRIO_CLASS_BE, 0)
        return _linux_set_nice(-5)
    return False


def set_utility() -> bool:
    """Set medium-low priority for supporting tasks."""
    global CURRENT_QOS
    CURRENT_QOS = "utility"
    if _lib_system:
        try:
            return _lib_system.pthread_set_qos_class_self_np(QOS_CLASS_UTILITY, 0) == 0
        except Exception:
            pass
    if _lib_c:
        _linux_set_ioprio(_IOPRIO_CLASS_BE, 6)
        return _linux_set_nice(10)
    return False


def set_background() -> bool:
    """Demote background research, learning, and backtest tasks so live trading always preempts them."""
    global CURRENT_QOS
    CURRENT_QOS = "background"
    if _lib_system:
        try:
            return _lib_system.pthread_set_qos_class_self_np(QOS_CLASS_BACKGROUND, 0) == 0
        except Exception:
            pass
    if _lib_c:
        if in_open_session():
            for policy_name in ("SCHED_IDLE", "SCHED_BATCH"):
                policy = getattr(os, policy_name, None)
                if policy is not None:
                    try:
                        os.sched_setscheduler(0, policy, os.sched_param(0))
                        break
                    except Exception:
                        continue
        _linux_set_ioprio(_IOPRIO_CLASS_IDLE, 7)
        return _linux_set_nice(19)
    return False
