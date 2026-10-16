output "instance_ids" {
  description = "Extra node instance ids. Empty when node_count is 0 or resources are disabled."
  value       = aws_instance.node[*].id
}
