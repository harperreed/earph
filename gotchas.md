# Gotchas

- On the tested macOS 27.2.1 host, registrations sharing one callback and a null client context collide with HAL `nope`.
  Keep a unique owned context for each registration, including silent probes.
  See [the Core Audio decision](docs/adr/0003-coreaudio-spike.md) and the real native ownership tests.
