# Quit stops playback

Date: November 21, 2026.
Status: Decided by Doctor Biz. Not yet implemented.

## Decision

When the user quits Earph, playback stops and the Engine exits.
No audio continues after Quit.
Earph offers no background-playback option.

Earph is an application, not an audio service.
A long-lived audio bus that other software plays through belongs to the operating system.
Earph does not build, install, or imitate one.

## Reason

Doctor Biz stated the rule on November 21, 2026:
"If we quit it should quit. No playing.
The idea of a service that can play things like an audio bus is better left up to the OS."

The rule matches what a listener expects from Quit.
It also removes a hidden state: an Engine that plays with no visible controller.

## Consequences

Quit stops the active source, closes the output, and shuts down the Engine owner.
The application then exits.
Quit follows the existing safe stop path; it adds no new audible policy.

The current app contradicts this decision.
It disconnects its controller and leaves the shared Engine running.
Change that behavior in a separate implementation task.

Disconnecting a controller without quitting is a different action.
[The desktop controller decision](0006-desktop-controller.md) still governs it.

Queue persistence and paused, muted crash recovery stay unchanged.
The next launch never resumes audio by itself.

## Assumptions to confirm

These points interpret the decision. Doctor Biz did not state them.

- The standalone Engine executable and CLI remain, as REL-03 requires.
  The user starts that process and stops it. Earph installs no launchd or systemd unit for it.
- Quit from the desktop app shuts down the Engine that the app attached to, including one started from the CLI.
- Closing a window is not Quit.
  [The specification](../SPEC.md) section 3.3 already says quitting the owner stops playback.
  It lets a closed window keep the host alive under a visible preference.
  This decision leaves that sentence unchanged.
