# network

VPC for `aws-staging`: public subnets for the ALB, private subnets for the center, optional nodes, and RDS.

Security groups match the tech draft sketch:

- ALB: TCP 443 and 80 from `alb_ingress_cidrs` only. An empty list creates no public rule. `0.0.0.0/0` is not a default.
- Center: TCP 80 from the ALB, TCP 50051 from the node security group. No SSH.
- RDS: TCP 3306 from the center security group only.
- Interface endpoints: TCP 443 from the VPC CIDR (SSM, SSM messages, EC2 messages).

An S3 gateway endpoint is attached to the private route table so a later artifact download does not need NAT. NAT is not in this skeleton. Management is SSM, not public SSH.

Every resource uses `count` or `for_each` and is created only when `enable_resources` is true.
