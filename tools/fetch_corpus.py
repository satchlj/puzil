"""Fetch public-channel message HTML from a Zulip server into a JSONL corpus.

Read-only: uses GET /messages with `channels:public`. Output (private content!) goes to
reference/, which is gitignored.

    python3 -I tools/fetch_corpus.py path/to/zuliprc reference/corpus.jsonl [max]
"""
import base64, configparser, json, sys, time, urllib.parse, urllib.request

rc = configparser.ConfigParser()
rc.read(sys.argv[1])
site, email, key = rc["api"]["site"].rstrip("/"), rc["api"]["email"], rc["api"]["key"]
out = sys.argv[2]
limit = int(sys.argv[3]) if len(sys.argv) > 3 else 200_000
auth = "Basic " + base64.b64encode(f"{email}:{key}".encode()).decode()

def get(path, **params):
    url = f"{site}/api/v1/{path}?" + urllib.parse.urlencode(params)
    while True:
        req = urllib.request.Request(url, headers={"Authorization": auth, "User-Agent": "puzil-dev"})
        try:
            return json.load(urllib.request.urlopen(req, timeout=60))
        except urllib.error.HTTPError as e:
            if e.code == 429:
                time.sleep(float(e.headers.get("Retry-After", 5)))
                continue
            raise

anchor, n = "newest", 0
with open(out, "w") as f:
    while n < limit:
        r = get("messages", anchor=anchor, num_before=5000, num_after=0, apply_markdown="true",
                narrow=json.dumps([{"operator": "channels", "operand": "public"}]))
        msgs = [m for m in r["messages"] if anchor == "newest" or m["id"] < anchor]
        for m in msgs:
            f.write(json.dumps({k: m[k] for k in ("id", "stream_id", "subject", "sender_full_name", "content")}) + "\n")
        n += len(msgs)
        print(f"{n} messages, oldest id {msgs[0]['id'] if msgs else '-'}", file=sys.stderr)
        if r.get("found_oldest") or not msgs:
            break
        anchor = msgs[0]["id"]
