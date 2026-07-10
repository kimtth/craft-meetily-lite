---
description: "Implementation guidance for a lightweight Windows-first Meetly transcription app"
---

# Lightweight Meetly Implementation Guide

Build a lightweight version of Meetly based on [ref-meetily](ref-meetily/). Keep the product focused on Windows meeting transcription with local, real-time transcription as the default workflow.

Do not modify anything in `ref-meetily/`. This directory is read-only and is provided for reference only.

## Required Workflow Before Implementation

Before starting any implementation work, follow this sequence:

1. Study the reference codebase in [ref-meetily](ref-meetily/) to understand the existing architecture, feature boundaries, dependencies, and relevant implementation patterns.
2. Prepare an implementation plan that analyzes the expected impact, required code changes, affected files, risks, and validation approach.
3. Share the plan with the user and wait for explicit approval.
4. Start implementation only after the user approves the plan.

## Core Principles

* Local-first by default: audio capture, recording, and Local Whisper transcription run on the user's machine.
* Azure Speech is an approved, optional cloud engine for live recognition and Fast Transcription of a selected WAV, MP3, or MP4 file. MP4 input is converted locally to MP3 before upload. The UI must clearly state before use that audio is sent directly to Azure Speech. Do not add Azure Blob Storage, SAS URLs, or storage-account keys.
* Real-time transcription: show the transcript while the meeting is in progress for Local Whisper and Azure Speech live recognition.

## UI Scope

Design the app as a compact side panel similar in size and behavior to the Microsoft Teams meeting side panel. Include only the UI elements needed for real-time transcription and the approved file-transcription workflow.

![Reference side panel](image.png)

The UI must support these workflows:

* View a list of recorded meetings.
* View a list of saved transcripts.
* Export recorded audio files.
* Export transcript files.

## AI Provider Scope

Do not expose AI provider settings unless the implementation includes a real feature that uses them. The lightweight app supports Local Whisper and the implemented Azure Speech live and Fast Transcription workflows.

If a future feature needs an AI provider, support only explicitly implemented local-first or approved providers. Do not add placeholder provider fields or decorative configuration UI.

## Out of Scope

Remove these features from the lightweight version:

* AI-powered meeting summaries
* Cross-platform support for macOS and Linux

Target Windows only.