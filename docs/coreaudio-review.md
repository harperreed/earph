# Core Audio spike review

Reviewed the T04 diff against `3c665da` on November 17, 2026.

An independent reviewer inspected native buffers, Core Foundation ownership, callback lifetime, precision assessment, redaction, dependencies, and SSH controls.
The reviewer found one material issue: normal remote-runner invocation did not force strict host-key checking.
The runner now forces that policy before both connection paths.
Connection-free tests query real OpenSSH configuration and verify both paths.
The same reviewer confirmed the fix and reported no remaining material findings.

The canonical checks passed on Linux and the native arm64 Mac without warnings.
Native tests exercised discovery, explicit inspection, registration, independent cleanup, and close/reopen.
CLI tests exercised real device reports and redaction.
The final width cases also test exact signed 16-, 24-, and 32-bit descriptors.
Clean-build evidence records the final suite counts and source commit.

Regression evidence records the raw-flag interpretation defect, null-context registration collision, and runner argument checks.
The fixes preserve one descriptor authority and one owned context per native registration.
No device-start, format-setting, gain-setting, or hog-acquisition operation exists in this spike.

The existing foundation archive and all 216 ALSA capture records still validate.
Dependency checks verify the binding publisher's exact license declaration and published VCS revision.
The publisher's SDK distribution question remains a release gate.

The source and docs scan found no credentials, personal device identifiers, private hostnames, or private network addresses.
The destination remains the intentionally public `harperreed/earph` repository.
Only redacted native observations enter the public evidence archive.
Private reports and failed-run logs remain in ignored task directories.

Physical playback, actual exclusive acquisition, format mutation, unplug, timing, and hardware preservation remain unqualified.
Native property races and failed IOProc destruction are handled conservatively but are not forced by the hardware tests.
Failed destruction retains callback data until process exit and reports failure.
