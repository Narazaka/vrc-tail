# vrc-tail

tail -f for VRChat logs

- reads recent set of logs (starts in 30 seconds)
  - output_log_2023-09-10_12-12-00.txt: read as `[3]`
  - output_log_2023-09-10_12-11-40.txt: read as `[2]`
  - output_log_2023-09-10_12-11-10.txt: read as `[1]`
  - output_log_2023-09-10_12-10-50.txt: read as `[0]`
  - output_log_2023-09-10_12-10-00.txt: ignore
- colored

![console](console.png)

## License

[Zlib License](LICENSE)
