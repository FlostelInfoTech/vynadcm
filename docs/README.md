# VynaDCM Documentation

Living documents for the VynaDCM software development cycle. Each
document carries its own version, date, and revision history, and is
updated with every release.

| Document | Audience | Description |
|---|---|---|
| [Requirements](requirements.md) | Product, QA, integrators | What the system must do (SRS with acceptance traceability) |
| [Design](design.md) | Developers, reviewers | How it is built: contracts, algorithms, sequences, invariants |
| [Integration guide](integration.md) | VynaECG / VynaLink developers | Dependency setup, recipes, TLS, plug-ins (snippets compile-checked) |
| [User guide](user-guide.md) | Operators, support | CLI reference, walkthroughs, troubleshooting |
| [SBOM](sbom.md) | Compliance, security | Bill of materials ([CycloneDX JSON](sbom.cyclonedx.json)) + regeneration |
| [Architecture](../ARCHITECTURE.md) | Everyone | One-page layer and module map |

Also at the repository root: [README](../README.md) (quick start),
[CHANGELOG](../CHANGELOG.md) (release notes).

## Document conventions

- Every document opens with a control table (version, date, status)
  and a revision history line.
- “Living draft” means the document tracks the main branch and may
  change without a version bump; releases freeze a versioned copy by
  tagging the repository.
- Code snippets in guides are verified: integration snippets are
  mirrored in a temporary example and `cargo check`ed; CLI text is
  transcribed from `--help` output.
