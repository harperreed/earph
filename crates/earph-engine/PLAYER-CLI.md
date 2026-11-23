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
That level suits headphones. It is hard to hear on speakers, and `play` and `sendspin` print a hint at -30 dB or lower.
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

## Pair with a server

The owner ran these steps against Music Assistant 2.10.13 from a Mac on November 22, 2026, and they worked.
Local test servers pass them, and so does the pinned aiosendspin 10.2.1 pairing server.
On a Mac, `scripts/sendspin-mac-test` runs the same steps with a dedicated keychain file.

macOS grants a stored identity to the exact helper executable that stored it.
A rebuilt `earph-credential-helper` reports the store as locked, although it is unlocked.
Keep the helper file that ran `init`, or pair again with the new one.

Run the four commands in order.

```sh
earph sendspin init
earph sendspin token
earph sendspin pair --server ws://SERVER-ADDRESS:8927/sendspin
earph sendspin --server ws://SERVER-ADDRESS:8927/sendspin --output "headphones"
```

On macOS, add `--credential-store KEYCHAIN-FILE` to every one of these commands.
The keychain file must already exist and be unlocked. No default keychain exists.

1. `init` creates this player's identity and its pairing secret in the credential store.
   It works only when the store holds no Sendspin identity. In every other case it refuses and changes nothing.
   It prints the player ID, which is public.
2. `token` prints the pairing token on standard output, and nothing else goes there.
   The token is permanent and secret: anyone who holds it can pair a server with this player at any time.
   Paste it into the server's player setup. Do not save it in a file, a script, or shell history.
3. `pair` connects to the server and waits up to 10 minutes for the server to pair.
   Enter the token in the server during that wait. `--timeout SECONDS` changes the wait.
   Music Assistant 2.10 keeps no token for a later connection, so start `pair` first.
   When the server completes pairing, `pair` stores the server's record, waits until the server switches to it, and disconnects.
   It prints `paired` and the server key. `--server-key` accepts that key.
4. `sendspin` then plays. Without `--server-key` it uses the one server that this player is paired with.

`pair` and `sendspin` use two separate connections. `pair` takes the same address rules as `sendspin`:
a numeric address, no `wss://`, and no loopback, link-local, or 100.64.0.0/10 address.
Use the server's local network address.

| Result of `init`, `token`, or `pair` | Exit status |
| --- | --- |
| done | 0 |
| the store refused, the server did not complete pairing, or the wait timed out | 1 |
| wrong usage or a refused address | 2 |
| `pair` interrupted by Control-C or SIGTERM | 130 |

If `pair` fails, it says what the store holds.
`Nothing was stored` means that the stored records did not change; run `pair` again.
After Control-C, `pair` still waits for a store write that already began, then reports the result.
If the store never confirmed a write, `pair` says that the store needs recovery.
Every later command then refuses that store, and Earph has no recovery command yet.

## Sendspin

```sh
earph sendspin --server ws://SERVER-ADDRESS:8927/sendspin --output "headphones"
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

While a stream is active and no audio is due, the player sends silence to keep the output running.
The output then runs before the first audio is due, so a slow device start does not delay the audio.
The status line leaves that silence out of the audio it counts.
When the server ends the stream, the player stops the output until the next stream starts.

A cleared stream, a gap in the stream, and an output underrun on Linux do not end the command.
The player continues with the next audio that the server sends.

| Flag | Meaning |
| --- | --- |
| `--server ws://IP:PORT/PATH` | The server. The address must be numeric; no name lookup happens. |
| `--server-key KEY` | The server's public key: 43 characters of URL-safe base64, the form of a Sendspin server ID. Optional when this player is paired with exactly one server. With no paired server, `sendspin` says to pair first. With several, it asks for this flag. |
| `--output NAME`, `--volume DB`, `--period`, `--buffer` | As for `play`. |
| `--credential-helper PATH` | The `earph-credential-helper` executable. The default is the file of that name beside `earph`. |
| `--credential-store STORE` | Where the player identity lives. On Linux: the D-Bus address of the keyring's bus; the default is `DBUS_SESSION_BUS_ADDRESS`. On macOS: the absolute path of a keychain file; no default exists. |
| `--credential-lock PATH` | The lock file that guards the store. The default is `sendspin-credentials.lock` in the runtime directory. Every command that uses the same store must use the same lock file. |

The Engine refuses loopback, link-local, 100.64.0.0/10, and other special-purpose addresses.
Use the server's local network address.
It also refuses `wss://`, because this player has no TLS.
`sendspin` checks the address and the key before it starts anything.

`sendspin` always starts its own Engine, because an Engine takes its server only when it starts.
If an Engine already runs at the runtime directory, `sendspin` refuses; run `earph quit` first.

From another terminal, `status`, `stop`, `mute`, `unmute`, `volume`, and `quit` work.
A live stream cannot pause, so `pause` stops a Sendspin stream, as `stop` does.
It prints `stopped (a Sendspin stream cannot pause)`, and the `sendspin` command ends with exit status 0.
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

Music Assistant 2.10.13 has driven this player on a Mac. Other versions are untested.
The store must hold a player identity that is paired with the server; see "Pair with a server".
If the credential store did not confirm an operation, `sendspin` says so in one line: run the command again.
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
