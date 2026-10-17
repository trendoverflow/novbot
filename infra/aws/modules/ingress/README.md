# ingress

HTTPS entry for C1. An internet-facing ALB terminates TLS with an ACM certificate. Port 80 only redirects to 443. The target group forwards HTTP to nginx on the center instance and checks `GET /health`.

The certificate domain defaults to `staging.example.invalid`. That name is a placeholder. Replace it with the staging domain Fei provides before any apply. This module does not create Route 53 records. DNS validation is a follow-up once the domain exists. A temporary staging certificate is acceptable if C1 can be tested.

Nothing here is created while `enable_resources` is false. Opening the ALB to the internet also requires a non-empty `alb_ingress_cidrs` on the network module. That list defaults to empty.
