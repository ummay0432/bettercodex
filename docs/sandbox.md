# Sandbox and permissions

BetterCodex does not include Codex's sandbox or approval-policy framework.
Commands and patches run with the permissions of the user who launched
`bcodex`, and they can read or modify anything that user can access.

Review the repository and its instructions before starting BetterCodex. Use a
separate operating-system account, container, or virtual machine when you need
an isolation boundary; BetterCodex does not create one for you.

The upstream product has a different security model. Do not assume the
official [Codex sandboxing and approvals](https://developers.openai.com/codex/security)
documentation describes BetterCodex behavior.
