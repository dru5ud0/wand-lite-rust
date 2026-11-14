# Notices and disclaimer

## Not affiliated

Wand Lite Rust ("Wand Lite") is an unofficial, independent project. It is **not
affiliated with, endorsed by, or supported by** Wand, WeMod, or any game
publisher. "Wand", "WeMod" and game titles are trademarks of their respective
owners and are used here only to describe interoperability.

## No third-party binaries or data are included

This repository contains **no** Wand/WeMod binaries, no trainer DLLs, no game
artwork, no catalog data, and no account data. It ships only original source
code plus the bundled Inter font (SIL Open Font License, see
`rust/ui/assets/fonts/LICENSE.txt`).

Everything proprietary is used at runtime from a copy the user has legitimately
installed:

| Runtime dependency | Where it comes from | Redistributed here? |
|---|---|---|
| `TrainerLib_{arch}.dll`, `CELib_{arch}.dll` | installed Wand application | no |
| `Trainer_<id>_<hash>.dll` trainer binaries | cached by the Wand application | no |
| Trainer blueprints (cheat names/targets) | Wand's local storage cache | no |
| Catalog metadata and game artwork | Wand's public CDN, fetched at runtime | no |

The application reads these files read-only. It does not modify the installed
Wand application, does not read or transmit account tokens, and does not bypass
licensing or entitlement checks.

## Intended use

* Use only with software you own and have a licence to run.
* Intended for single-player / offline use. Using trainers in multiplayer or
  competitive modes can violate a game's terms of service and may result in
  account action. You are responsible for how you use it.
* The protocol descriptions in `docs/` were derived from interoperability
  analysis of a locally installed copy and are provided to document and enable
  compatibility. They are not a distribution of the original software.

## No warranty

The software is provided "as is", without warranty of any kind. Injecting code
into another process can crash that process; see `LICENSE`.

## If you are a rights holder

If you believe this repository contains material that should not be published,
please open an issue and it will be removed promptly. The project deliberately
contains no proprietary binaries or assets.
