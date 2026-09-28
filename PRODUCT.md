# Product

## Register

product

## Users

Developers using Compi as a daily native terminal workspace on Windows with WSL2 or on macOS. They need active shells, sessions, tabs, and panes to persist independently of any client window, with fast keyboard-first access and platform-native behavior.

## Product Purpose

Compi provides a native cross-platform terminal workspace backed by a persistent server. It should make process persistence, multiplexing, reconnection, configuration, and maintenance feel like ordinary terminal behavior rather than infrastructure the user must manage.

## Brand Personality

Native, calm, precise. Compi communicates with quiet confidence, concise language, and enough technical detail to support informed decisions without interrupting active work.

## Anti-references

Avoid gamer-terminal styling, decorative SaaS dashboards, gratuitous motion, verbose onboarding, hidden lifecycle consequences, and modal-heavy maintenance flows. Do not make routine product maintenance look urgent when it is not.

## Design Principles

1. Protect running work. Client actions must never imply process termination unless they actually terminate daemon-owned work, and destructive effects must be explicit.
2. Stay out of the terminal’s way. Surface status and maintenance actions where users expect them, then return focus to active work.
3. Prefer native, familiar behavior. Follow host-platform interaction, keyboard, focus, and installation conventions instead of inventing custom system affordances.
4. Explain consequences, not implementation trivia. Copy should state what changes, what keeps running, and when a restart is required.
5. Keep system state inspectable. Connection, update, failure, and recovery states must be visible and actionable without becoming a dashboard.

## Accessibility & Inclusion

Target WCAG 2.2 AA contrast. Support complete keyboard operation, visible focus, reduced motion, and status cues that never rely on color alone. Preserve usable layouts under scaling and narrow window sizes, and use concise labels that remain understandable outside visual context.
