# Public entry for C1: ALB terminates HTTPS, port 80 redirects to 443,
# and the target group talks HTTP to nginx on the center instance.
# ACM stays a DNS-validated certificate request. This module does not
# create DNS records and does not wait on validation.

resource "aws_acm_certificate" "this" {
  count = var.enable_resources ? 1 : 0

  domain_name       = var.certificate_domain_name
  validation_method = "DNS"

  lifecycle {
    create_before_destroy = true
  }

  tags = {
    Name = "${var.name_prefix}-cert"
  }
}

resource "aws_lb" "this" {
  count = var.enable_resources ? 1 : 0

  name                       = "${var.name_prefix}-alb"
  load_balancer_type         = "application"
  internal                   = false
  security_groups            = var.alb_security_group_ids
  subnets                    = var.public_subnet_ids
  drop_invalid_header_fields = true

  tags = {
    Name = "${var.name_prefix}-alb"
  }
}

resource "aws_lb_target_group" "center" {
  count = var.enable_resources ? 1 : 0

  name     = "${var.name_prefix}-tg"
  port     = 80
  protocol = "HTTP"
  vpc_id   = var.vpc_id

  health_check {
    path                = "/health"
    protocol            = "HTTP"
    matcher             = "200"
    healthy_threshold   = 2
    unhealthy_threshold = 2
    interval            = 30
  }

  tags = {
    Name = "${var.name_prefix}-tg"
  }
}

resource "aws_lb_target_group_attachment" "center" {
  count = var.enable_resources ? 1 : 0

  target_group_arn = aws_lb_target_group.center[0].arn
  target_id        = var.center_instance_id
  port             = 80
}

resource "aws_lb_listener" "http" {
  count = var.enable_resources ? 1 : 0

  load_balancer_arn = aws_lb.this[0].arn
  port              = 80
  protocol          = "HTTP"

  default_action {
    type = "redirect"

    redirect {
      port        = "443"
      protocol    = "HTTPS"
      status_code = "HTTP_301"
    }
  }
}

resource "aws_lb_listener" "https" {
  count = var.enable_resources ? 1 : 0

  load_balancer_arn = aws_lb.this[0].arn
  port              = 443
  protocol          = "HTTPS"
  ssl_policy        = "ELBSecurityPolicy-TLS13-1-2-2021-06"
  certificate_arn   = aws_acm_certificate.this[0].arn

  default_action {
    type             = "forward"
    target_group_arn = aws_lb_target_group.center[0].arn
  }
}
