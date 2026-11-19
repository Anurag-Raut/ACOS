#!/usr/bin/env bash
R=results/handoff/GATENE2E/run-arm.sh
$R gate-gaten-bubble   scenarios/gaten-bubble.yaml
$R gate-a11y-probe     scenarios/a11y-probe.yaml
$R gate-a11y-wrap      scenarios/a11y-screenshot-wrap.yaml
$R gate-gles-software  scenarios/gles-software.yaml
$R gate-counter        scenarios/counter.yaml
$R gate-form           scenarios/form-validation.yaml
$R gate-webview-probe  scenarios/webview-probe.yaml
