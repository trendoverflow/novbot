# github_oidc

GitHub Actions identity for this repository. No static AWS keys.

When `enable_resources` is later turned on, this module creates:

- An OIDC provider for `https://token.actions.githubusercontent.com`
- `novbot-gha-terraform-plan`, trusted only for the plan subject (default: pull requests)
- `novbot-gha-terraform-apply`, trusted only for GitHub Environment `aws-staging`

Both roles can call `sts:GetCallerIdentity` and nothing else. Plan and apply permissions have to be written, reviewed by NovSec, and approved by Fei before a workflow can change AWS. Do not attach `AdministratorAccess`.

Repositories that use GitHub's immutable subject claims (owner id and repository id in `sub`) must override `plan_subject` and `apply_subject` before GO. Do not replace those strings with a wildcard.

Thumbprints are public certificate identifiers, not secrets. Re-check them at bootstrap.
