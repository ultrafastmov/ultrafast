import json
import time
import unittest

from ultrafastmov import WebhookVerificationError, webhooks

SECRET = "whsec_6c1f00"


def delivery(sent_at):
    body = json.dumps({"id": "job_1", "status": "rendered", "event": "job.rendered", "sent_at": sent_at, "receipt": {"url": "https://x/y.mp4"}}).encode()
    return body, webhooks.sign(body, SECRET)


class VerifyTest(unittest.TestCase):
    def test_good(self):
        body, sig = delivery("2026-11-21T10:00:00.123Z")
        now = 1795255200.0  # 2026-11-21T10:00:00Z
        job = webhooks.verify(body, sig, SECRET, now=now)
        self.assertEqual(job["id"], "job_1")
        self.assertEqual(webhooks.verify(body.decode(), sig.upper().replace("SHA256=", "sha256="), SECRET, now=now)["status"], "rendered")
        # several signatures (secret rotation): any match passes
        self.assertEqual(webhooks.verify(body, "sha256=00, " + sig, SECRET, now=now)["id"], "job_1")

    def test_bad_signatures(self):
        body, sig = delivery("2026-11-21T10:00:00Z")
        for header in (None, "", "sha256=", sig[:-1] + ("0" if sig[-1] != "0" else "1"), "md5=" + sig[7:], sig + "00", "sha256=é"):
            with self.assertRaises(WebhookVerificationError, msg=header):
                webhooks.verify(body, header, SECRET, tolerance=None)
        with self.assertRaises(WebhookVerificationError):
            webhooks.verify(body, sig, "whsec_other", tolerance=None)
        with self.assertRaises(WebhookVerificationError):
            webhooks.verify(body, sig, "", tolerance=None)

    def test_age(self):
        now = time.time()
        stamp = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(now - 600))
        body, sig = delivery(stamp)
        with self.assertRaises(WebhookVerificationError):
            webhooks.verify(body, sig, SECRET)
        self.assertEqual(webhooks.verify(body, sig, SECRET, tolerance=900)["id"], "job_1")
        self.assertEqual(webhooks.verify(body, sig, SECRET, tolerance=None)["id"], "job_1")
        body, sig = delivery("2026-11-21T10:00:00.123456789Z")  # nanoseconds
        self.assertEqual(webhooks.verify(body, sig, SECRET, now=1795255200.5)["id"], "job_1")
        raw = json.dumps({"id": "job_1"}).encode()
        with self.assertRaises(WebhookVerificationError):
            webhooks.verify(raw, webhooks.sign(raw, SECRET), SECRET)
        raw = b"[1, 2]"
        with self.assertRaises(WebhookVerificationError):
            webhooks.verify(raw, webhooks.sign(raw, SECRET), SECRET, tolerance=None)


if __name__ == "__main__":
    unittest.main()
