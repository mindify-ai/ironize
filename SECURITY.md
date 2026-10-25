# Security policy

## Reporting a vulnerability

Please do not open a public issue for security problems. Report them privately
through [GitHub's private vulnerability reporting](https://github.com/mindify-ai/ironize/security/advisories/new).

Include the affected version or commit, steps to reproduce, and the impact you
expect. We aim to acknowledge reports within three business days and will keep
you updated until a fix is released.

## Supported versions

Security fixes go into the latest release only.

## Verifying a release

Every release binary ships with a `SHA256SUMS` file and a signed build
provenance attestation. To check that a download was built by this repository's
release workflow:

```sh
sha256sum --check --ignore-missing SHA256SUMS
gh attestation verify ironize-<version>-<target>.tar.gz --repo mindify-ai/ironize
```
