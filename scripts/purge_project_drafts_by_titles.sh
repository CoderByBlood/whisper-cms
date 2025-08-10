#!/bin/bash

python3 purge_project_drafts_by_titles.py \
  --owner CoderByBlood \
  --project-number 2 \
  --csv whispercms_project_backlog.csv \
  --delete
#  --dry-run