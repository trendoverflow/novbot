# compute_center

One EC2 instance in a private subnet for the first-land shape: `novbot-center`, nginx, the console, and a co-located `novbot-node` (product minimum of one node, C4).

No SSH key. The instance profile attaches `AmazonSSMManagedInstanceCore`. IMDSv2 is required. The root volume is encrypted.

`user_data` is a marker script. It does not embed a Bearer token, a database URL, or an OrbStack demo secret. systemd units and the pinned tip are installed later by the deploy workflow.

The cloud nginx listen address is not the OrbStack loopback `:80`. The ALB targets instance port 80. That nginx change is a deploy concern, not this stub.

Created only when `enable_resources` is true. Set `ami_id` before that plan (arm64 for `t4g`, x86_64 for `t3`).
