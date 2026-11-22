# `earph`: play a file from the command line

`earph` plays local files through the Engine and reports what happened.
Use it to test real playback without the desktop application.

## Play

```sh
earph devices
earph play track.flac --output "headphones"
```

`--output` takes any part of an output name from `earph devices`, or an exact endpoint.
Naming an output approves audio on that output. Set `EARPH_OUTPUT` to name it once per shell.
If you name no output and `earph devices` lists exactly one, `play` uses and approves that output.
If it lists several, `play` refuses and prints them.
`play` accepts several files and plays them in order.
It prints the output, the volume, each track, and the transport with its position and sample rates.
Press Control-C to stop.

The volume changes only when you pass `--volume`, in dB from -120 to 0.
A new Engine starts at -40 dB.
That level suits headphones. It is hard to hear on speakers, and `play` prints a hint at -30 dB or lower.
On speakers, try `--volume -10`.

`play` ends with one `result` line and an exit status.

| Result | Exit status |
| --- | --- |
| `finished`: the last file reached its end | 0 |
| `stopped`: another command stopped playback | 0 |
| playback failed, the Engine rejected a file, or nothing is playing | 1 |
| wrong usage, unreadable path, or no output chosen | 2 |
| the Engine refused a command or is unreachable | 3 |
| `interrupted` by Control-C or SIGTERM | 130 |

If an output fails, the Engine prints the device detail as a `backend_fault` JSON line on standard error.

## Control a running play from another terminal

```sh
earph status
earph pause
earph resume
earph volume -30
earph seek 1:30
earph mute
earph unmute
earph stop
earph quit
```

`seek` takes whole seconds or `M:SS` and refuses a position past the end of the file.
`pause`, `resume`, `stop`, and `seek` return after the playback worker acknowledges the change.
`status --json` prints the complete Engine state.

## Which Engine it uses

`earph` uses the runtime directory in `--runtime`, then `EARPH_RUNTIME`, then `~/.earph-cli`.
This default is separate from the desktop application's Engine, so its queue and volume stay untouched.
If no Engine runs at that directory, `play` starts one inside its own process and stops it on exit.
If an Engine already runs there, `play` uses it and leaves it running.

`devices` also starts a temporary Engine when none runs, and stops it afterward.

`play` removes its own queue entries when it ends.
If that Engine is playing or its queue holds files, `play` refuses.
Pass `--replace-queue` to stop that playback and remove those files first.
On failure, `play` stops only the playback that it started.
A `play` that started its own Engine stops that Engine when its playback ends.
To replace such a play, run `earph stop` first, then `earph play` again.

## Tuning

`--period FRAMES` and `--buffer FRAMES` set the output period and buffer.
The defaults are 64 and 256 frames on Linux, and 64 and 4096 frames on macOS.
