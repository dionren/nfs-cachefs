#!/bin/bash
echo "Testing environment..."
echo "Current directory: $(pwd)"
echo "Git status:"
git status
echo "Docker version:"
docker --version
echo "GitHub CLI version:"
gh --version
echo "Files in current directory:"
ls -la 