import argparse
import json
from pathlib import Path
import subprocess
import sys


def case(case_id, packet, split=0, fetch="x", data="r", width="s"):
    return {
        "id": case_id,
        "packet": packet,
        "split": split,
        "fetch": fetch,
        "data": data,
        "width": width,
    }


def cases():
    yield case("control-addss-register", "f30f58c0")
    yield case("control-addsd-register", "f20f58c0", width="d")
    yield case("control-addss-memory", "f30f5807")
    yield case("control-addsd-memory", "f20f5807", width="d")
    yield case("control-ud2", "0f0b")
    yield case("control-cli", "fa")
    yield case("control-nop-15", "66" * 14 + "90")
    yield case("control-nop-16", "66" * 15 + "90")
    yield case("control-fetch-unmapped", "0f1fc0", 1, "u")
    yield case("control-fetch-nx", "0f1fc0", 1, "n")
    yield case("control-fetch-rx", "0f1fc0", 1)
    yield case("control-data-guard", "f30f5807", data="g")

    for prefix, width in (("f0f2", "d"), ("f2f0", "d"), ("f0f3", "s"), ("f3f0", "s")):
        for name, opcode in (
            ("add-register", "0f58c0"),
            ("add-memory", "0f5807"),
            ("cmp-immediate", "0fc284370000000000"),
            ("pshuf-immediate", "0f708437000000001b"),
        ):
            packet = prefix + opcode
            stem = f"{prefix}-{name}"
            yield case(stem + "-full", packet, width=width)
            if name != "add-register":
                yield case(stem + "-guard", packet, data="g", width=width)
            for split in range(1, len(packet) // 2):
                for fetch in ("u", "n", "x"):
                    yield case(f"{stem}-split-{split}-{fetch}", packet, split, fetch, width=width)
            yield case(stem + "-exact-end", packet, len(packet) // 2, "u", width=width)

        for length in (15, 16):
            packet = "66" * (length - 5) + prefix + "0f58c0"
            yield case(f"{prefix}-length-{length}-full", packet, width=width)
            for split in (14, 15):
                for fetch in ("u", "n", "x"):
                    stem = f"{prefix}-length-{length}-split-{split}-{fetch}"
                    yield case(stem, packet, split, fetch, width=width)

        if width == "s":
            yield case(f"{prefix}-pause", prefix + "90")
            yield case(f"{prefix}-tzcnt", prefix + "0fbcc0")


def probe(binary, *args, timeout=10):
    result = subprocess.run(
        [binary, *args],
        stdin=subprocess.DEVNULL,
        capture_output=True,
        check=True,
        text=True,
        timeout=timeout,
    )
    return json.loads(result.stdout)


def control_failures(rows, translated):
    controls = {row["id"]: row["observation"] for row in rows if row["id"].startswith("control-")}
    failures = []
    expected = {
        "control-addss-register": {"result": "ret", "xmm0": "4130000040e0000040a0000040800000"},
        "control-addsd-register": {"result": "ret", "xmm0": "40140000000000004010000000000000"},
        "control-addss-memory": {"result": "ret", "xmm0": "4130000040e0000040a0000040a00000"},
        "control-addsd-memory": {"result": "ret", "xmm0": "40140000000000004014000000000000"},
        "control-ud2": {"result": "SIGILL", "pc_delta": 0},
        "control-nop-15": {"result": "ret"},
    }
    if not translated:
        expected["control-ud2"].update(trap=6, err=0)
        expected["control-cli"] = {"trap": 13, "err": 0, "pc_delta": 0}
        expected["control-nop-16"] = {"trap": 13, "err": 0, "pc_delta": 0}
        expected["control-fetch-rx"] = {"result": "ret"}
        expected["control-fetch-unmapped"] = {
            "trap": 14, "err": 20, "pc_delta": 0, "fault_addr_delta": 1, "si_addr_delta": 1,
        }
        expected["control-fetch-nx"] = {
            "trap": 14, "err": 21, "pc_delta": 0, "fault_addr_delta": 1, "si_addr_delta": 1,
        }
        data_guard = controls["control-data-guard"]
        data_delta = data_guard.get("data_delta", 0)
        if data_guard.get("err", 16) & 16 or data_delta <= 0:
            failures.append({"id": "control-data-guard", "expected": "operand read fault"})
        expected["control-data-guard"] = {
            "trap": 14, "pc_delta": 0, "fault_addr_delta": data_delta, "si_addr_delta": data_delta,
        }
    failures.extend(
        {"id": name, "field": key, "expected": value, "actual": controls[name].get(key)}
        for name, fields in expected.items()
        for key, value in fields.items()
        if controls[name].get(key) != value
    )
    return failures


def main():
    parser = argparse.ArgumentParser(description="observe locked mandatory-prefix encodings")
    parser.add_argument("probe", type=Path)
    binary = str(parser.parse_args().probe.resolve())
    identity = probe(binary, "--identity")
    translated = identity["translated"] == 1
    rows = []
    for row in cases():
        # a fresh executable process prevents translated code from a prior mapping being reused.
        args = [row["packet"], str(row["split"]), row["fetch"], row["data"], row["width"]]
        try:
            observation = probe(binary, *args, timeout=2 if translated else 10)
        except subprocess.TimeoutExpired:
            if not translated:
                raise
            observation = {"translated": 1, "page": identity["page"], "result": "timeout"}
        rows.append({**row, "observation": observation})
    failures = control_failures(rows, translated)
    report = {
        "platform": sys.platform, "identity": identity, "control_failures": failures, "rows": rows,
    }
    print(json.dumps(report, indent=2))
    return bool(failures)


if __name__ == "__main__":
    sys.exit(main())
