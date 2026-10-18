# compute_node

Optional extra node EC2 instances. They are not required for the first landing: C4 is met by the node on the center instance.

`node_count` defaults to 0. With that default the module creates no role and no instance, including when `enable_resources` is true.

Instances land in private subnets, with SSM and IMDSv2, and no public IP or SSH key. They dial center gRPC on port 50051. The security group rule for that path lives in the network module.

`user_data` does not contain a bootstrap token. The token shell is `novbot/<environment>/bootstrap-token` in the secrets module.
