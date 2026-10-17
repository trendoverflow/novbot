# Center EC2: novbot-center, nginx, console, and one co-located novbot-node.
# user_data does not install the release and does not embed a token.
# The deploy workflow (after Fei GO) delivers artifacts and restarts systemd.
# ami_id stays null in git. The instance count is 0 until enable_resources
# is true, and a plan with that flag set fails until ami_id is a real AMI.

data "aws_iam_policy_document" "ec2_assume" {
  count = var.enable_resources ? 1 : 0

  statement {
    actions = ["sts:AssumeRole"]

    principals {
      type        = "Service"
      identifiers = ["ec2.amazonaws.com"]
    }
  }
}

resource "aws_iam_role" "center" {
  count = var.enable_resources ? 1 : 0

  name               = "${var.name_prefix}-center"
  assume_role_policy = data.aws_iam_policy_document.ec2_assume[0].json

  tags = {
    Name = "${var.name_prefix}-center"
  }
}

resource "aws_iam_role_policy_attachment" "ssm" {
  count = var.enable_resources ? 1 : 0

  role       = aws_iam_role.center[0].name
  policy_arn = "arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore"
}

resource "aws_iam_instance_profile" "center" {
  count = var.enable_resources ? 1 : 0

  name = "${var.name_prefix}-center"
  role = aws_iam_role.center[0].name
}

resource "aws_instance" "center" {
  count = var.enable_resources ? 1 : 0

  ami           = var.ami_id
  instance_type = var.instance_type
  subnet_id     = var.subnet_ids[0]

  lifecycle {
    precondition {
      condition     = can(regex("^ami-[0-9a-f]{8,17}$", var.ami_id))
      error_message = "Set ami_id to a real AMI before enable_resources can create the center instance."
    }
  }

  vpc_security_group_ids      = var.vpc_security_group_ids
  iam_instance_profile        = aws_iam_instance_profile.center[0].name
  associate_public_ip_address = false

  metadata_options {
    http_endpoint               = "enabled"
    http_tokens                 = "required"
    http_put_response_hop_limit = 1
  }

  root_block_device {
    encrypted   = true
    volume_type = "gp3"
    volume_size = 30
  }

  user_data = <<-EOT
    #!/bin/bash
    # Skeleton boot script. It does not install NovBot and it does not read a token.
    # After Fei GO, deploy-ec2 delivers the pinned tip and restarts:
    #   novbot-center, novbot-node, nginx
    echo "novbot center skeleton tip ${var.tip_sha}"
    EOT

  tags = {
    Name   = "${var.name_prefix}-center"
    TipSha = var.tip_sha
    Role   = "center"
  }
}
