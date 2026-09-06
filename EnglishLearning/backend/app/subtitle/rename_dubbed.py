#!/usr/bin/env python3
"""Script to rename all mp4 files by removing '_dubbed' and '_sync_dubbed' suffixes."""

import os

# Base directory
base_dir = "/home/shang/Desktop/FinallyMicroService/services/EnglishLearning/backend/app/subtitle"

# Counter for renamed files
renamed_count = 0

# Walk through all subdirectories
for root, dirs, files in os.walk(base_dir):
    for filename in files:
        # Process only mp4 files
        if filename.endswith(".mp4"):
            # Check if filename contains '_dubbed' or '_sync_dubbed'
            if "_sync_dubbed" in filename:
                new_filename = filename.replace("_sync_dubbed", "")
                old_path = os.path.join(root, filename)
                new_path = os.path.join(root, new_filename)
                os.rename(old_path, new_path)
                print(f"Renamed: {filename}")
                print(f"  To: {new_filename}")
                print()
                renamed_count += 1
            elif "_dubbed" in filename:
                new_filename = filename.replace("_dubbed", "")
                old_path = os.path.join(root, filename)
                new_path = os.path.join(root, new_filename)
                os.rename(old_path, new_path)
                print(f"Renamed: {filename}")
                print(f"  To: {new_filename}")
                print()
                renamed_count += 1

print(f"Total files renamed: {renamed_count}")
