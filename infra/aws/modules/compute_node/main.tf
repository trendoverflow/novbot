# Optional extra nodes (orb-arm-2/3 alignment). The co-located node on the
# center instance is the C4 minimum and does not use this module.
# Default node_count is 0, so this module creates nothing even if the
# root enable_resources flag is later turned on without raising the count.

locals {
  create = var.enable_resources && var.node_count > 0
}

data "aws_iam_policy_document" "ec2_assume" {
  count = local.create ? 1 : 0

  statement {
    actions = ["sts:AssumeRole"]

    principals {
      type        = "Service"
      identifiers = ["ec2.amazonaws.com"]
    }
  }
}

resource "aws_iam_role" "node" {
  count = local.create ? 1 : 0

  name               = "${var.name_prefix}-node"
  assume_role_policy = data.aws_iam_policy_document.ec2_assume[0].json

  tags = {
    Name = "${var.name_prefix}-node"
  }
}

resource "aws_iam_role_policy_attachment" "ssm" {
  count = local.create ? 1 : 0

  role       = aws_iam_role.node[0].name
  policy_arn = "arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore"
}

resource "aws_iam_instance_profile" "node" {
  count = local.create ? 1 : 0

  name = "${var.name_prefix}-node"
  role = aws_iam_role.node[0].name
}

resource "aws_instance" "node" {
  count = local.create ? var.node_count : 0

  ami           = var.ami_id
  instance_type = var.instance_type
  subnet_id     = var.subnet_ids[count.index % length(var.subnet_ids)]

  lifecycle {
    precondition {
      condition     = can(regex("^ami-[0-9a-f]{8,17}$", var.ami_id))
      error_message = "Set ami_id to a real AMI before creating extra node instances."
    }
  }

  vpc_security_group_ids      = var.vpc_security_group_ids
  iam_instance_profile        = aws_iam_instance_profile.node[0].name
  associate_public_ip_address = false

  metadata_options {
    http_endpoint               = "enabled"
    http_tokens                 = "required"
    http_put_response_hop_limit = 1
  }

  root_block_device {
    encrypted   = true
    volume_type = "gp3"
    volume_size = 20
  }

  user_data = <<-EOT
    #!/bin/bash
    # Skeleton boot script. It does not install NovBot and it does not read a token.
    # After Fei GO, deploy-ec2 delivers the pinned tip and restarts novbot-node.
    echo "novbot node skeleton tip ${var.tip_sha}"
    EOT

  tags = {
    Name   = "${var.name_prefix}-node-${count.index + 1}"
    TipSha = var.tip_sha
    Role   = "node"
  }
}
