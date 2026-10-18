# VPC, subnets, security groups, and SSM/S3 endpoints.
# for_each / count stay empty while enable_resources is false.

locals {
  public_subnets = var.enable_resources ? {
    for idx, az in var.availability_zones : az => var.public_subnet_cidrs[idx]
  } : {}

  private_subnets = var.enable_resources ? {
    for idx, az in var.availability_zones : az => var.private_subnet_cidrs[idx]
  } : {}

  ssm_services = toset(["ssm", "ssmmessages", "ec2messages"])
}

resource "aws_vpc" "this" {
  count = var.enable_resources ? 1 : 0

  cidr_block           = var.vpc_cidr
  enable_dns_support   = true
  enable_dns_hostnames = true

  tags = {
    Name = var.name_prefix
  }
}

resource "aws_internet_gateway" "this" {
  count = var.enable_resources ? 1 : 0

  vpc_id = aws_vpc.this[0].id

  tags = {
    Name = var.name_prefix
  }
}

resource "aws_subnet" "public" {
  for_each = local.public_subnets

  vpc_id                  = aws_vpc.this[0].id
  cidr_block              = each.value
  availability_zone       = each.key
  map_public_ip_on_launch = true

  tags = {
    Name = "${var.name_prefix}-public-${each.key}"
    Tier = "public"
  }
}

resource "aws_subnet" "private" {
  for_each = local.private_subnets

  vpc_id                  = aws_vpc.this[0].id
  cidr_block              = each.value
  availability_zone       = each.key
  map_public_ip_on_launch = false

  tags = {
    Name = "${var.name_prefix}-private-${each.key}"
    Tier = "private"
  }
}

resource "aws_route_table" "public" {
  count = var.enable_resources ? 1 : 0

  vpc_id = aws_vpc.this[0].id

  tags = {
    Name = "${var.name_prefix}-public"
  }
}

resource "aws_route" "public_default" {
  count = var.enable_resources ? 1 : 0

  route_table_id         = aws_route_table.public[0].id
  destination_cidr_block = "0.0.0.0/0"
  gateway_id             = aws_internet_gateway.this[0].id
}

resource "aws_route_table_association" "public" {
  for_each = aws_subnet.public

  subnet_id      = each.value.id
  route_table_id = aws_route_table.public[0].id
}

resource "aws_route_table" "private" {
  count = var.enable_resources ? 1 : 0

  vpc_id = aws_vpc.this[0].id

  tags = {
    Name = "${var.name_prefix}-private"
  }
}

resource "aws_route_table_association" "private" {
  for_each = aws_subnet.private

  subnet_id      = each.value.id
  route_table_id = aws_route_table.private[0].id
}

resource "aws_vpc_endpoint" "s3" {
  count = var.enable_resources ? 1 : 0

  vpc_id            = aws_vpc.this[0].id
  service_name      = "com.amazonaws.${var.aws_region}.s3"
  vpc_endpoint_type = "Gateway"
  route_table_ids   = [aws_route_table.private[0].id]

  tags = {
    Name = "${var.name_prefix}-s3"
  }
}

resource "aws_security_group" "alb" {
  count = var.enable_resources ? 1 : 0

  name        = "${var.name_prefix}-alb"
  description = "Public HTTPS entry and HTTP redirect."
  vpc_id      = aws_vpc.this[0].id

  tags = {
    Name = "${var.name_prefix}-alb"
  }
}

resource "aws_security_group" "center" {
  count = var.enable_resources ? 1 : 0

  name        = "${var.name_prefix}-center"
  description = "Center EC2. HTTP from the ALB, gRPC from node instances. No SSH."
  vpc_id      = aws_vpc.this[0].id

  tags = {
    Name = "${var.name_prefix}-center"
  }
}

resource "aws_security_group" "node" {
  count = var.enable_resources ? 1 : 0

  name        = "${var.name_prefix}-node"
  description = "Optional extra node EC2. Egress only; no SSH."
  vpc_id      = aws_vpc.this[0].id

  tags = {
    Name = "${var.name_prefix}-node"
  }
}

resource "aws_security_group" "rds" {
  count = var.enable_resources ? 1 : 0

  name        = "${var.name_prefix}-rds"
  description = "MySQL from the center instance only."
  vpc_id      = aws_vpc.this[0].id

  tags = {
    Name = "${var.name_prefix}-rds"
  }
}

resource "aws_security_group" "endpoints" {
  count = var.enable_resources ? 1 : 0

  name        = "${var.name_prefix}-endpoints"
  description = "HTTPS from the VPC to interface VPC endpoints."
  vpc_id      = aws_vpc.this[0].id

  tags = {
    Name = "${var.name_prefix}-endpoints"
  }
}

resource "aws_security_group_rule" "alb_https" {
  for_each = var.enable_resources ? toset(var.alb_ingress_cidrs) : toset([])

  type              = "ingress"
  from_port         = 443
  to_port           = 443
  protocol          = "tcp"
  cidr_blocks       = [each.value]
  security_group_id = aws_security_group.alb[0].id
  description       = "HTTPS from an approved staging CIDR"
}

resource "aws_security_group_rule" "alb_http" {
  for_each = var.enable_resources ? toset(var.alb_ingress_cidrs) : toset([])

  type              = "ingress"
  from_port         = 80
  to_port           = 80
  protocol          = "tcp"
  cidr_blocks       = [each.value]
  security_group_id = aws_security_group.alb[0].id
  description       = "HTTP redirect to HTTPS from an approved staging CIDR"
}

resource "aws_security_group_rule" "center_http_from_alb" {
  count = var.enable_resources ? 1 : 0

  type                     = "ingress"
  from_port                = 80
  to_port                  = 80
  protocol                 = "tcp"
  source_security_group_id = aws_security_group.alb[0].id
  security_group_id        = aws_security_group.center[0].id
  description              = "nginx from the ALB only"
}

resource "aws_security_group_rule" "center_grpc_from_nodes" {
  count = var.enable_resources ? 1 : 0

  type                     = "ingress"
  from_port                = 50051
  to_port                  = 50051
  protocol                 = "tcp"
  source_security_group_id = aws_security_group.node[0].id
  security_group_id        = aws_security_group.center[0].id
  description              = "gRPC from optional node instances"
}

resource "aws_security_group_rule" "rds_mysql_from_center" {
  count = var.enable_resources ? 1 : 0

  type                     = "ingress"
  from_port                = 3306
  to_port                  = 3306
  protocol                 = "tcp"
  source_security_group_id = aws_security_group.center[0].id
  security_group_id        = aws_security_group.rds[0].id
  description              = "MySQL from the center instance only"
}

resource "aws_security_group_rule" "endpoints_https_from_vpc" {
  count = var.enable_resources ? 1 : 0

  type              = "ingress"
  from_port         = 443
  to_port           = 443
  protocol          = "tcp"
  cidr_blocks       = [var.vpc_cidr]
  security_group_id = aws_security_group.endpoints[0].id
  description       = "HTTPS from the VPC to SSM endpoints"
}

resource "aws_vpc_endpoint" "ssm" {
  for_each = var.enable_resources ? local.ssm_services : toset([])

  vpc_id              = aws_vpc.this[0].id
  service_name        = "com.amazonaws.${var.aws_region}.${each.value}"
  vpc_endpoint_type   = "Interface"
  subnet_ids          = [for subnet in aws_subnet.private : subnet.id]
  security_group_ids  = [aws_security_group.endpoints[0].id]
  private_dns_enabled = true

  tags = {
    Name = "${var.name_prefix}-${each.value}"
  }
}
