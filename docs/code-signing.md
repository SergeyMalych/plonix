# Code signing policy

The Windows installer, `Plonix-Windows-setup.exe`, is code-signed so Windows can show who published it and that it was not changed after it was built.

Free code signing provided by [SignPath.io](https://about.signpath.io), certificate by [SignPath Foundation](https://signpath.org).

## What is signed

- Only builds made by the [release workflow](https://github.com/SergeyMalych/plonix/blob/main/.github/workflows/release.yml) on GitHub-hosted runners, from a version tag on this repository, are sent for signing. Nothing built on a personal computer is signed.
- Every signing request is approved by a maintainer before the certificate is used.
- What is signed is built only from the source in this repository and the open-source dependencies listed in `Cargo.lock`.
- On Windows the installer, `Plonix-Windows-setup.exe`, is the file that is signed.
- The Mac app is signed separately, with the maintainers' Apple certificate.

Updates are signed a second time with the Plonix update key, and Plonix refuses an update whose signature does not match. See [Updates](updates.md).

## Team roles

| Role | Who |
| --- | --- |
| Authors and committers | [Sergey Malych](https://github.com/SergeyMalych), and anyone the maintainers give write access to the repository |
| Reviewers | [Sergey Malych](https://github.com/SergeyMalych) |
| Approvers | [Sergey Malych](https://github.com/SergeyMalych) |

Outside contributions arrive as pull requests from forks and are reviewed by a maintainer before they are merged. Every change reaches `main` through a pull request with passing checks, and `main` is protected. Maintainers use multi-factor authentication for GitHub and SignPath.

## Privacy

This program will not transfer any information to other networked systems unless specifically requested by the user or the person installing or operating it. Usage statistics are off unless you turn them on, update checks happen only when you allow them, and crash reports are sent only when you choose to. [Privacy](privacy.md) lists exactly what each one sends.

## Reporting a problem

If a signed Plonix file looks wrong, or you think the signing was misused, report it as described in [SECURITY.md](https://github.com/SergeyMalych/plonix/blob/main/SECURITY.md).
