# Private local test media

Keep private audio corpora and archives under the repository-root `media-ignore/` directory.
Git ignores that directory; do not force-add its contents.
The supplied corpus does not grant redistribution rights for its copyrighted recordings.
Do not copy its media, track metadata, or provenance into public evidence or source bundles.

Media may exist only in the primary checkout.
An isolated worktree does not copy untracked files from another checkout.
Use an explicit local path when a development tool needs the corpus.
Preserve original files and tags.

Read each corpus's README and manifest before using it.
The supplied manifest records encoded-file hashes, decoded PCM hashes, exact frame counts, and equivalence groups.
It also distinguishes lossless content, lossy codecs, DSD payloads, and DoP carriers.
DoP carriers must never enter an ordinary PCM DSP path.

Use WAV, FLAC, and AIFF for the first decoder acceptance cases in T06.
Compare equivalent lossless files by numeric samples and frame counts.
Compare concatenated gapless segments against their continuous reference.
Qualify lossy, extended containers, multichannel, and DSD support separately.
Do not promote corpus integrity checks to player or hardware qualification.

Keep generated, redistributable fixtures as the public CI baseline.
Private media must not become a required dependency of `scripts/check`.
Hash and decode checks may run offline without physical output.
Signal-emitting hardware tests still require explicit approval for the selected output.
