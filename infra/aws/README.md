# NovBot AWS skeleton (no apply)

EC2 + systemd Terraform for **aws-staging**. This tree is a skeleton. It does not create AWS resources.

| | |
|---|---|
| Code tip | `7b3f6913` (`7b3f6913e8ffd2d7ec1e1ccfa38ca583616b438a`) |
| Product scope | `trendoverflow/novbot-enterprise` `docs/2026-10-16-novbot-aws-mvp-deploy-scope.md` @ `6458724` §6 |
| Tech draft | `trendoverflow/novbot-enterprise` `docs/2026-10-16-novbot-aws-ec2-terraform-gha-draft.md` @ `44bb37a` |
| Compute | EC2 + systemd (Fei). Not ECS or EKS. |

## GO gate

**Fei must approve in chat before any `terraform apply`, before any bootstrap of state or OIDC, and before any EC2 deploy.** A green CI run is not approval. A GitHub Environment approval is not approval.

Until that GO:

- `enable_resources` stays `false` (the default in `variables.tf` and in `envs/aws-staging.tfvars.example`).
- The S3 + DynamoDB backend in `backend.tf` stays commented out.
- [`.github/workflows/terraform-apply.yml`](../../.github/workflows/terraform-apply.yml) is `workflow_dispatch` only and exits 1.
- [`.github/workflows/deploy-ec2.yml`](../../.github/workflows/deploy-ec2.yml) is `workflow_dispatch` only and exits 1.
- Do not run `terraform apply` or `terraform plan` against a real AWS account from a laptop.

Applying this stack later bills the account (EC2, RDS, ALB, public IPv4, interface VPC endpoints, Secrets Manager). The skeleton itself does not.

## What validate does today

From this directory, with no AWS credentials:

```bash
terraform fmt -check -recursive
terraform init -backend=false -input=false
terraform validate
```

`terraform init -backend=false` downloads the AWS provider and does not configure a remote backend. `.terraform.lock.hcl` is committed so CI installs the same provider. It holds hashes, not credentials. `terraform validate` checks the configuration. It does not create resources. The plan workflow runs those three commands on pull requests that touch `infra/aws/**`. If AWS credentials are missing, that is the expected case: fmt, init, and validate still run.

`terraform plan` is not part of the workflow. Do not add it until the backend exists and Fei has approved a real plan.

## How plan and apply will work after Fei GO

This is the intended sequence. None of it is enabled in this skeleton.

1. Fei states GO for aws-staging bootstrap.
2. Create a new S3 bucket and a DynamoDB lock table for this project. Uncomment the backend block in `backend.tf`, replace every `REPLACE_ME`, and do not commit an account id. State is encrypted in S3. The DynamoDB table is the lock. State files stay out of git (C9).
3. Apply is still not the next command. First replace the GitHub OIDC role policies under NovSec review. The roles in `modules/github_oidc` can only call `sts:GetCallerIdentity`. Plan must stay read-only. Apply must not be `AdministratorAccess`.
4. In the GitHub repository settings, Environment **`aws-staging`** gets required reviewers. That setting is not in the workflow YAML. `terraform-apply.yml` and `deploy-ec2.yml` already name the environment so a later real job stops for those reviewers.
5. Point the plan workflow at `novbot-gha-terraform-plan` with GitHub OIDC (`id-token: write`). No long-lived access keys in the workflow or in repository secrets.
6. A pull request that changes `infra/aws/**` runs `terraform plan` and posts the diff. It still does not apply.
7. A maintainer runs **Terraform apply** with `workflow_dispatch`. The Environment reviewers approve, and Fei's GO is already on record. Only then does the workflow run `terraform apply`.
8. **Deploy EC2** is a second dispatch: build tip `7b3f6913` (or a later tip Fei names), upload the artifact to S3, SSM Run Command to install it and restart `novbot-center`, `novbot-node`, and `nginx`. No SSH.

Laptop `terraform apply` bypasses the Environment reviewers. After GO, apply stays on the workflow.

## Layout

```text
infra/aws/
  versions.tf
  providers.tf
  backend.tf          # example only, commented out
  variables.tf
  outputs.tf
  main.tf
  envs/aws-staging.tfvars.example
  modules/
    network/          # VPC, subnets, security groups, SSM + S3 endpoints
    data/             # RDS MySQL 8
    compute_center/   # center EC2 + co-located node
    compute_node/     # optional extra nodes (count defaults to 0)
    ingress/          # ALB + ACM
    secrets/          # secret shells, no values
    github_oidc/      # plan role and apply role
```

Copied tfvars are gitignored. Commit only `*.tfvars.example`.

## C1–C9 ownership

Product still Pass/Fails these after a real apply. This table says where the skeleton points. Checks are not executed here.

| ID | Product check | Owner in this tree |
|---|---|---|
| C1 | HTTPS console, certificate valid | `modules/ingress` (ALB + ACM, HTTP redirects to 443). Domain and client CIDR are Fei inputs. `alb_ingress_cidrs` defaults to empty, so the ALB security group has no public rule until a CIDR is set. |
| C2 | `/health` `git_sha` prefix `7b3f6913` | `tip_sha` (default `7b3f6913`). Deploy workflow input defaults to the same pin. The center and node binaries already embed the git SHA. |
| C3 | Missing token is 401; cloud Bearer works | `modules/secrets` shell `novbot/aws-staging/api-token`. No secret version in Terraform. nginx and `/v1` are a deploy concern. |
| C4 | At least one node with a fresh last_seen | Co-located node on `modules/compute_center`. `modules/compute_node` is optional (`node_count` default 0). |
| C5 | Skills Hub shows the seed set | Not Terraform. A later seed script must list the actual skills if the set is smaller than the demo. |
| C6 | Dispatch `spec_id: cpu` and get an ack | Post-deploy smoke (later). At-least-once dispatch is already on tip `7b3f6913`. |
| C7 | `/v1/ee/*` returns 402, not 5xx | OSS center behavior. Do not set a license key to hide 402. Smoke is later. |
| C8 | OrbStack database and secrets are untouched | This tree has no OrbStack paths. Workflows do not target the demo. Cloud state, RDS, and secrets are separate. |
| C9 | No apply without Fei GO; state and lock explained | This README, the commented backend, and `terraform-apply.yml` (dispatch only, exit 1). |

## Secrets

Never put these in git, in workflow YAML, or in a committed tfvars file:

- AWS account ids
- access keys or secret keys
- Bearer tokens, bootstrap tokens, database passwords
- OrbStack demo tokens

**Do not reuse OrbStack demo token names or values.** That includes `demo-api-token-change-me`. Cloud auth is a new Bearer in Secrets Manager. The RDS master password is created by RDS (`manage_master_user_password`) and is not a Terraform argument.

`providers.tf` skips STS and IMDS checks so validate does not need credentials. Turn `skip_aws_api_checks` off only for a plan Fei has approved.

## Out of this skeleton

ECS, EKS, production, a Cloud Access Card, NAT, Route 53 records, and real IAM policies for plan/apply. Package egress from private subnets (no NAT) is an open follow-up; SSM and the S3 gateway endpoint are the paths this stub does include.
