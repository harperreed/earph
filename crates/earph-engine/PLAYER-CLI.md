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

## Sendspin

```sh
earph sendspin --server ws://SERVER-ADDRESS:8927/sendspin --server-key SERVER-KEY --output "headphones"
```

`sendspin` starts an Engine for one Sendspin server, connects to it as a player, and plays what the server sends.
It chooses and approves the output as `play` does, and it changes the volume only when you pass `--volume`.
It prints the output, then one status line: connecting, waiting for the server to start a stream, or playing.
That line also shows how much audio went to the output, the volume, and `muted` while muted.

The line always says `sync unqualified`.
The player schedules audio against the server's clock, but no measurement shows when sound leaves the output.
Expect a constant offset from other players, and slow drift.
`start error` is the player's own estimate, not a measurement.
The command shows no playback position for the same reason.

| Flag | Meaning |
| --- | --- |
| `--server ws://IP:PORT/PATH` | The server. The address must be numeric; no name lookup happens. |
| `--server-key KEY` | The server's public key: 43 characters of URL-safe base64, the form of a Sendspin server ID. |
| `--output NAME`, `--volume DB`, `--period`, `--buffer` | As for `play`. |
| `--credential-helper PATH` | The `earph-credential-helper` executable. The default is the file of that name beside `earph`. |
| `--credential-store STORE` | Where the player identity lives. On Linux: the D-Bus address of the keyring's bus; the default is `DBUS_SESSION_BUS_ADDRESS`. On macOS: the absolute path of a keychain file; no default exists. |
| `--credential-lock PATH` | The lock file that guards the store. The default is `sendspin-credentials.lock` in the runtime directory. Every Engine that uses the same store must use the same lock file. |

The Engine refuses loopback, link-local, 100.64.0.0/10, and other special-purpose addresses.
Use the server's local network address.
It also refuses `wss://`, because this player has no TLS.
`sendspin` checks the address and the key before it starts anything.

`sendspin` always starts its own Engine, because an Engine takes its server only when it starts.
If an Engine already runs at the runtime directory, `sendspin` refuses; run `earph quit` first.

From another terminal, `status`, `stop`, `mute`, `unmute`, `volume`, and `quit` work.
A live stream cannot pause, so `pause` stops a Sendspin stream, as `stop` does.
An Engine that cannot do that refuses the pause; then use `stop` or `mute`.
`resume` and `seek` are not supported for a Sendspin stream.

`sendspin` ends with one `result` line and an exit status.

| Result | Exit status |
| --- | --- |
| `ended`: the server closed the connection, or another command stopped the stream or the Engine | 0 |
| the stream failed, or the credential store or the server denied access | 1 |
| wrong usage, a refused address or key, or an Engine already runs | 2 |
| the Engine refused a command | 3 |
| `interrupted` by Control-C or SIGTERM | 130 |

The keyring or keychain must already be unlocked.
The helper never prompts, unlocks, or creates a store.
Nothing reconnects: after the connection ends, run the command again.

Real Music Assistant use does not work yet.
The store must already hold a player identity that is paired with the server, and no command creates or pairs one.
Those pairing commands are planned work.
Until then, `sendspin` fails with `AUTH_REQUIRED` against a store that holds no identity.
The player also takes no volume or mute commands from the server, and it offers only 48 kHz stereo 16-bit PCM.

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
