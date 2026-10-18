# secrets

Empty Secrets Manager shells for aws-staging:

- `novbot/<environment>/api-token` (C3 HTTP Bearer)
- `novbot/<environment>/bootstrap-token` (node registration)
- `novbot/<environment>/database-url` (center DSN; password is not chosen here)

No `aws_secretsmanager_secret_version` resources. Turning `enable_resources` on later creates the containers only. A human writes the values after Fei GO, outside git.

Do not copy OrbStack demo token names or values into these shells. The banned demo name includes `demo-api-token-change-me`. The RDS master password is a different secret, created by RDS itself (`manage_master_user_password` in the data module).

The default AWS-managed key encrypts the shells. A customer-managed key can wait for the NovSec review.
