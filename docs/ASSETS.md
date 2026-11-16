# Binary asset storage

GLB meshes and WAV/MP3 audio are tracked with Git LFS through `.gitattributes`.
Run `git lfs install --local` and `git lfs pull` after cloning. CI checks out LFS
content before compilation because the car and submarine demos embed their GLBs
with `include_bytes!`. Working files remain real binary files; Git stores small
pointers and LFS stores the payloads.

The migration applies to current and future commits. Historical Git objects and
existing tags are retained; there is no force push or history rewrite. Use
`git lfs ls-files` and `git lfs fsck` to inspect the current objects. Contributors
must keep the LFS pre-push hook enabled so payloads upload before Git references.

Source archives used by Homebrew or other packagers must contain materialized
assets. Each release attaches `nAIVE-X.Y.Z-source.tar.gz`, built by the release
workflow from a checkout after `git lfs pull`, and the Homebrew formula builds
from it. GitHub's automatic archives can still hold LFS pointers.
Verify the files start with GLB/audio signatures, not the LFS pointer header,
before publishing an archive. Plain archive downloads are not a replacement for
`git lfs pull` unless this setting is enabled.

Generated assets stay ignored unless intentionally added as demo fixtures. Keep
large generated collections outside this repository, for example as versioned
release asset packs with checksums. Do not rewrite public history to reclaim old
binary storage as part of routine maintenance.
