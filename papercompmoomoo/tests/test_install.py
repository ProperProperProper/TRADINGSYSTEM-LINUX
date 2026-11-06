import plistlib
import unittest
from pathlib import Path

from papercompmoomoo.install import (
    JOBS,
    OMLX_LABEL,
    PREFIX,
    job_plist,
    job_service,
    omlx_plist,
    omlx_service,
    qwen_service,
)

PROJECT = Path("/p/my & project")
PYTHON = PROJECT / ".venv/bin/python"


class Plist(unittest.TestCase):
    def test_linux_qwen_service_uses_llama_cpp_loopback_and_the_existing_model(self):
        service = qwen_service(Path("/opt/llama/llama-server"), Path("/models/savant.gguf"))
        self.assertIn('ExecStart="/opt/llama/llama-server"', service)
        self.assertIn('"/models/savant.gguf"', service)
        self.assertIn('"127.0.0.1"', service)
        self.assertIn('"qwen2.5-7b-instruct"', service)
        self.assertIn('Environment="LD_LIBRARY_PATH=/opt/llama"', service)
        self.assertIn("Restart=always", service)
    def test_every_job_is_valid_xml_for_this_checkout_and_kept_alive(self):
        for job, (module, *_rest) in JOBS.items():
            data = plistlib.loads(job_plist(job, PROJECT, PYTHON).encode())
            self.assertEqual(data["Label"], PREFIX + job)
            self.assertEqual(data["ProgramArguments"], [str(PYTHON), "-m", module])
            self.assertEqual(data["WorkingDirectory"], "/p/my & project")
            self.assertEqual(data["EnvironmentVariables"], {"PYTHONPATH": "src"})
            self.assertTrue(data["KeepAlive"] and data["RunAtLoad"])

    def test_logs_go_where_the_monitoring_commands_look(self):
        sup = plistlib.loads(job_plist("supervisor", PROJECT, PYTHON).encode())
        self.assertEqual((sup["StandardOutPath"], sup["StandardErrorPath"]), ("/tmp/papercompmoomoo-supervisor.log", "/tmp/papercompmoomoo-supervisor.err"))
        self.assertNotIn("Nice", sup)

    def test_the_commander_runs_as_its_own_always_on_job_and_the_local_model_server_is_loopback_only_with_a_key_and_a_memory_ceiling(self):
        self.assertIn("standby", JOBS)
        data = plistlib.loads(omlx_plist(Path("/home/u"), "secret&key").encode())
        self.assertEqual(data["Label"], OMLX_LABEL)
        args = data["ProgramArguments"]
        self.assertEqual(args[0], "/home/u/.omlx/venv/bin/omlx")
        self.assertEqual(args[args.index("--host") + 1], "127.0.0.1", "never reachable from the network")
        self.assertEqual(data["EnvironmentVariables"], {"OMLX_API_KEY": "secret&key"})
        self.assertEqual(args[args.index("--memory-guard-gb") + 1], "7")
        self.assertEqual(args[args.index("--max-concurrent-requests") + 1], "1", "one request at a time: the 16 GB machine aborted requests under memory pressure with more, and decoding is memory-bound so more gives no speed")
        self.assertTrue(data["KeepAlive"] and data["RunAtLoad"])
        self.assertEqual(data["ProcessType"], "Background")

    def test_systemd_units_match_the_launchd_jobs_and_quote_paths(self):
        project = Path("/p/my 100% project")
        python = project / ".venv/bin/python"
        for job, (module, *_rest) in JOBS.items():
            svc = job_service(job, project, python)
            self.assertIn(f"Description=papercompmoomoo {PREFIX}{job}\n", svc)
            self.assertIn(f'ExecStart="/p/my 100%% project/.venv/bin/python" "-m" "{module}"\n', svc)
            self.assertIn("WorkingDirectory=/p/my 100%% project\n", svc)
            self.assertIn("Environment=PYTHONPATH=src\n", svc)
            self.assertIn("Restart=always\n", svc)
            self.assertIn("[Install]\nWantedBy=default.target\n", svc)
        standby_svc = job_service("standby", project, python)
        self.assertIn("Nice=10\n", standby_svc)
        self.assertIn("StandardOutput=append:/tmp/papercompmoomoo-standby.log\n", standby_svc)
        self.assertIn("StandardError=append:/tmp/papercompmoomoo-standby.err\n", standby_svc)

        omlx = omlx_service(Path("/home/u"), "secret%key")
        self.assertIn('Environment="OMLX_API_KEY=secret%%key"\n', omlx)
        self.assertIn('"127.0.0.1"', omlx)
        self.assertIn("Restart=always\n", omlx)


if __name__ == "__main__":
    unittest.main()
