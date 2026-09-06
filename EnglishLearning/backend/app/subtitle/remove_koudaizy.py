#!/usr/bin/env python3
"""Script to remove '[koudaizy.com]--' from all filenames in the subtitle directory."""

import os

base_dir = "/home/shang/Desktop/FinallyMicroService/services/EnglishLearning/backend/app/subtitle"

renamed_count = 0

for root, dirs, files in os.walk(base_dir):
    for filename in files:
        if "--[koudaizy.com]" in filename:
            new_filename = filename.replace("--[koudaizy.com]", "")
            old_path = os.path.join(root, filename)
            new_path = os.path.join(root, new_filename)
            os.rename(old_path, new_path)
            print(f"Renamed: {filename}")
            print(f"  To: {new_filename}")
            print()
            renamed_count += 1

print(f"Total files renamed: {renamed_count}")
