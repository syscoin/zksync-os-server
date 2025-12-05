import re
import sys


def parse_run_file(path):
    pattern = re.compile(
        r'Main block for (\(.+?\)):\s*Some\((\d+)\)'
    )
    results = []

    with open(path, "r") as f:
        for line in f:
            m = pattern.search(line)
            if m:
                key = m.group(1)
                block_number = int(m.group(2))
                results.append((key, block_number))

    return results


def parse_server_log(path):
    pattern = re.compile(
        r'computaional_native_used:\s*(\d+).*?block_number=(\d+)'
    )

    mapping = {}

    with open(path, "r") as f:
        for line in f:
            m = pattern.search(line)
            if m:
                used = int(m.group(1))
                block = int(m.group(2))
                mapping[block] = used

    return mapping


def parse_prover_log(path):
    pattern = re.compile(
        r'batch number\s+(\d+).*?generated in\s+([\d\.]+)\s+seconds'
    )

    mapping = {}

    with open(path, "r") as f:
        for line in f:
            m = pattern.search(line)
            if m:
                batch = int(m.group(1))
                seconds = float(m.group(2))
                mapping[batch] = seconds

    return mapping


def main():
    if len(sys.argv) != 4:
        print("Usage: python parse_logs.py run.txt server_log.txt prover.log")
        return

    run_path = sys.argv[1]
    server_path = sys.argv[2]
    prover_path = sys.argv[3]

    run_entries = parse_run_file(run_path)
    server_entries = parse_server_log(server_path)
    prover_entries = parse_prover_log(prover_path)

    for key, block_num in run_entries:
        used = server_entries.get(block_num)
        seconds = prover_entries.get(block_num)

        if used is not None and seconds is not None:
            native_per_ms = used / (seconds * 1000)
            npm_str = f"{native_per_ms:.4f}"
        else:
            npm_str = "N/A"

        used_str = used if used is not None else "NOT FOUND"
        sec_str = seconds if seconds is not None else "NOT FOUND"

        print(
            f'Flow: {key}. Block number: {block_num}. '
            f'computaional_native_used: {used_str}. '
            f'seconds: {sec_str}. '
            f'native/ms: {npm_str}'
        )


if __name__ == "__main__":
    main()
