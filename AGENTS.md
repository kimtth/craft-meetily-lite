---
description: "Implementation guidance for a lightweight Windows-first Meetly transcription app"
---

# Lightweight Meetly Implementation Guide

Build a lightweight version of Meetly based on [ref-meetily](ref-meetily/). Keep the product focused on local, real-time meeting transcription for Windows.

Do not modify anything in `ref-meetily/`. This directory is read-only and is provided for reference only.

## Required Workflow Before Implementation

Before starting any implementation work, follow this sequence:

1. Study the reference codebase in [ref-meetily](ref-meetily/) to understand the existing architecture, feature boundaries, dependencies, and relevant implementation patterns.
2. Prepare an implementation plan that analyzes the expected impact, required code changes, affected files, risks, and validation approach.
3. Share the plan with the user and wait for explicit approval.
4. Start implementation only after the user approves the plan.

## Core Principles

* Local-first processing: all audio capture, recording, and transcription must run on the user's machine. Meeting data must not leave the computer.
* Real-time transcription: show the transcript while the meeting is in progress.

## UI Scope

Design the app as a compact side panel similar in size and behavior to the Microsoft Teams meeting side panel. Include only the UI elements needed for real-time transcription.

![Reference side panel](image.png)

The UI must support these workflows:

* View a list of recorded meetings.
* View a list of saved transcripts.
* Export recorded audio files.
* Export transcript files.

## AI Provider Scope

Do not expose AI provider settings unless the implementation includes a real feature that uses them. The lightweight app currently supports local transcription through Whisper model files only.

If a future feature needs an AI provider, support only explicitly implemented local-first or approved providers. Do not add placeholder provider fields or decorative configuration UI.

## Out of Scope

Remove these features from the lightweight version:

* AI-powered meeting summaries
* Cross-platform support for macOS and Linux

Target Windows only.