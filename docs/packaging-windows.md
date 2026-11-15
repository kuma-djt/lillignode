# Packaging Lillignode for Windows

How the Windows release folder is built, and what the script refuses to package.

## What it makes

One folder, `dist\lillignode-<version>-windows-x64\`, holding everything a Windows user needs:

| File | What it is |
|---|---|
| `lillignode.exe` | Lillignode, built with `cargo build --release --locked` |
| `conpty.dll`, `OpenConsole.exe` | Microsoft's newer pseudo-terminal. Without them beside `lillignode.exe`, Claude Code's status cannot reach Lillignode and every agent shows Unknown |
| `LICENSE-Microsoft-ConPTY.txt` | The MIT notice for those two files, word for word from the Windows Terminal repository at the same release. It says at the top that it does not cover Lillignode |
| `README.md` | The README |
| `LICENSE` | Lillignode's own licence, the AGPL-3.0 |

`dist\` is not committed.

## How to run it

From the repository, in any terminal:

```
powershell -ExecutionPolicy Bypass -File scripts\package-windows.ps1
```

It needs the Rust toolchain for x64 Windows and, the first time, a connection to github.com. It takes about a minute. It ends with the folder's path and the SHA-256 hash of every file in it, or with `PACKAGING FAILED` and exit code 1.

Two options:

- `-Package <file>` uses a copy of the Microsoft package already on the machine and downloads nothing. The copy is checked exactly as a downloaded one is.
- `-BesideBuild` also places the two checked Microsoft files beside `target\release\lillignode.exe`, so a Lillignode run straight from the build folder shows status, and the end-to-end run (`cargo run --release --example e2e`) takes its newer-pseudo-terminal path. To go back to the built-in one, delete `target\release\conpty.dll` and `target\release\OpenConsole.exe`.

## What it checks, and what it refuses

The Microsoft files come from one pinned package, the one Lillignode was checked by hand with: `Microsoft.Windows.Console.ConPTY.1.25.261109001.nupkg`, attached to the Windows Terminal `v1.25.3131.0` release on GitHub. The download is kept in `target\package-cache\` and reused.

| Check | Refused when |
|---|---|
| The package's SHA-256 hash | The download, or the copy given with `-Package`, is not byte for byte the pinned package. A kept copy that no longer matches is thrown away and fetched again |
| `conpty.dll` SHA-256 hash | It is not the file whose hash is written in the script |
| `OpenConsole.exe` SHA-256 hash | It is not the file whose hash is written in the script |
| `conpty.dll` signature | Windows does not report the signature as valid, or the signer is not Microsoft Corporation |
| `OpenConsole.exe` signature | The same |
| The packaged `lillignode.exe --version` | It does not start or does not report a version. It is run with `LILLIGNODE_STATE_DIR` pointed at a throwaway folder |

The hash check and the signature check do not depend on each other. The hash is compared with a value written in the script; the signature is checked by Windows against Microsoft's certificate and does not read that value. A wrong pin in the script is caught by the signature; a file signed by Microsoft but not the version that was hand-checked is caught by the hash.

Everything is assembled in a hidden folder inside `dist\` and moved into place only after every check has passed. A refused run writes no release folder, leaves a release folder from an earlier run as it was, and says so. The two Microsoft files are checked once more in the release folder after the move.

## Checking that it still refuses

```
powershell -ExecutionPolicy Bypass -File scripts\test-package-windows.ps1
```

This builds damaged copies of the package in a throwaway folder (one byte changed in each Microsoft file in turn, one file left out) and confirms each is refused for the right reason, that a changed file fails its signature as well as its hash, that an unsigned program is refused, and that the whole script given a changed package exits with code 1, says what was wrong, and leaves `dist\` exactly as it was. It downloads nothing; run the packaging script once first.

Run it after any change to `scripts\package-windows.ps1`.

## Moving to a newer Microsoft package

The pin is five values at the top of `scripts\package-windows.ps1`: the release, the package name, and three hashes. Changing them is a decision, not a chore: every agent runs inside these two files. Record the new version and hashes in the script and in this page, run the test script, and repeat the status hand check against real Claude Code before shipping.

## Not done, not checked

- **The folder holds the program, not its source.** Lillignode is under the AGPL-3.0; anyone given the folder must also be told where the source is. The README in the folder links to it.
- **x64 only.** The package also holds arm64 and x86 files; the script refuses to run on anything but x64.
- **`lillignode.exe` is not signed.** Windows may warn when a downloaded copy is first run.
- **No installer and no archive.**
- **Not checked:** a file validly signed by someone other than Microsoft (none was to hand; the script compares the signer's name); a failed download (the message is written, the path was not exercised); PowerShell 7 (run under Windows PowerShell 5.1 only); the release folder used by hand on a machine that never built Lillignode.
