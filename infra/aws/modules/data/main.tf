# RDS MySQL 8. The master password is managed by RDS in Secrets Manager
# (manage_master_user_password). This module has no password argument and
# creates no secret version.

resource "aws_db_subnet_group" "this" {
  count = var.enable_resources ? 1 : 0

  name       = "${var.name_prefix}-mysql"
  subnet_ids = var.private_subnet_ids

  tags = {
    Name = "${var.name_prefix}-mysql"
  }
}

resource "aws_db_instance" "mysql" {
  count = var.enable_resources ? 1 : 0

  identifier     = "${var.name_prefix}-mysql"
  engine         = "mysql"
  engine_version = var.engine_version
  instance_class = var.instance_class

  allocated_storage = var.allocated_storage_gb
  storage_type      = "gp3"
  storage_encrypted = true

  db_name  = "novbot"
  username = var.username

  manage_master_user_password = true

  db_subnet_group_name   = aws_db_subnet_group.this[0].name
  vpc_security_group_ids = var.vpc_security_group_ids
  publicly_accessible    = false
  multi_az               = var.multi_az

  backup_retention_period = var.backup_retention_days
  copy_tags_to_snapshot   = true
  deletion_protection     = false
  skip_final_snapshot     = true

  auto_minor_version_upgrade = true

  tags = {
    Name = "${var.name_prefix}-mysql"
  }
}
