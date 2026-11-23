# ABOUTME: Tests the parts of the Sendspin loopback capture driver that need no Mac: head start, usage, and refusals.
# ABOUTME: Runs on any machine with Python 3.9 or later: python3 -m unittest test_loopback_capture.py
import contextlib
import io
import pathlib
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import loopback_capture as capture  # noqa: E402


def run(*arguments):
    """Runs the driver and returns its exit status and its error output."""
    errors = io.StringIO()
    with contextlib.redirect_stderr(errors), contextlib.redirect_stdout(io.StringIO()):
        status = capture.main(list(arguments))
    return status, errors.getvalue()


class LoopbackCapture(unittest.TestCase):
    def test_the_head_start_follows_the_pinned_server_formula(self):
        declared = {"min_buffer_ms": 200, "required_lead_time_ms": 150, "output_delay_ms": 0}
        self.assertEqual(capture.server_send_ahead_us(declared), 200_000)
        # A device latency of 160 ms raises both declared values.
        declared = {"min_buffer_ms": 360, "required_lead_time_ms": 310, "output_delay_ms": 0}
        self.assertEqual(capture.server_send_ahead_us(declared), 360_000)
        declared = {"min_buffer_ms": 100, "required_lead_time_ms": 150, "output_delay_ms": 20}
        self.assertEqual(capture.server_send_ahead_us(declared), 170_000)
        declared = {"min_buffer_ms": 0, "required_lead_time_ms": 0, "output_delay_ms": 0}
        self.assertEqual(capture.server_send_ahead_us(declared), 50_000)

    def test_usage_errors_exit_with_status_2_and_show_the_usage(self):
        for arguments in ([], ["bogus", "--root", "/"], ["--bogus"], ["run", "--root", "/"], ["analyze", "name"]):
            status, errors = run(*arguments)
            self.assertEqual(status, 2, arguments)
        self.assertIn("usage: loopback_capture.py", errors)
        self.assertEqual(run("--help")[0], 0)

    def test_a_work_folder_that_others_can_read_is_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            pathlib.Path(directory).chmod(0o755)
            status, errors = run("--root", directory, "run", "name")
            self.assertEqual(status, 3)
            self.assertIn("mode 700", errors)

    def test_analyze_names_a_missing_run(self):
        with tempfile.TemporaryDirectory() as directory:
            status, errors = run("--root", directory, "analyze", "absent")
            self.assertEqual(status, 3)
            self.assertIn("run.json", errors)


if __name__ == "__main__":
    unittest.main()
