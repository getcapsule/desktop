Synthetic AAC regression fixtures: two seconds of 440 Hz followed by two
seconds of 880 Hz, at 48 kHz mono. No recorded music is included.

Both files are embedded in the test binary; CI does not need FFmpeg.
Regenerate from this directory with FFmpeg:

ffmpeg -f lavfi -i sine=frequency=440:sample_rate=48000:duration=2 -f lavfi -i sine=frequency=880:sample_rate=48000:duration=2 -filter_complex "[0:a][1:a]concat=n=2:v=0:a=1" -c:a aac -b:a 64k -movflags +faststart -y resume-two-tones.m4a
ffmpeg -i resume-two-tones.m4a -c:a copy -movflags +frag_keyframe+empty_moov+default_base_moof -frag_duration 500000 -y resume-two-tones-fragmented.m4a
