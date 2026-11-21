// Type-level checks against the built declarations (`npm run typecheck`).
import {
  type AnyNode,
  type Document,
  type Node,
  type ParamsOf,
  Image,
  Job,
  Render,
  Ultrafast,
  isOp,
  verifyWebhook,
} from "ultrafastmov";

type Equal<A, B> = (<T>() => T extends A ? 1 : 2) extends <T>() => T extends B ? 1 : 2 ? true : false;
const assertType = <T extends true>(_: T) => {};

export async function siteSample() {
  const uf = new Ultrafast(); // ULTRAFAST_API_KEY

  const doc = await uf.load("film.json");
  const sheet = await uf.sheet(doc, { count: 9 });
  const job = await uf.render(doc, {
    webhook: "https://example.com/rendered",
  });
  console.log(job.id, job.status);

  assertType<Equal<typeof sheet, Image>>(true);
  assertType<Equal<typeof job, Job>>(true);
  assertType<Equal<Awaited<ReturnType<typeof uf.new>>, Document>>(true);
  const render = await uf.render(doc);
  assertType<Equal<typeof render, Render>>(true);
  const polled = await uf.render(doc, { job: true });
  assertType<Equal<typeof polled, Job>>(true);
  const done: Render = await polled.wait({ pollMs: 1000, timeoutMs: 60_000 });
  await done.save("film.mp4");
  // @ts-expect-error jobs cannot be inline
  await uf.render(doc, { webhook: "https://example.com", inline: true });
  console.log(String(await uf.estimate(doc)));
  for (const issue of await uf.review(doc, { safe: "social" })) console.log(issue.kind, issue.node, issue.message);
  const event = await verifyWebhook("{}", "sha256=00", "whsec_x");
  assertType<Equal<typeof event.status, "queued" | "rendering" | "rendered" | "failed" | "canceled">>(true);
}

export const film: Document = {
  version: 1,
  format: { width: 1080, height: 1920, fps: 30, duration: 8, motion_blur: { samples: 6, shutter: 0.5 } },
  nodes: {
    bg: { op: "gradient", params: { kind: "radial", a: "#101418", b: [0.1, 0.1, 0.2, 1] } },
    title: {
      op: "text",
      params: {
        text: "Hello *world*",
        font: "Inter",
        size: 96,
        color: "#ffffff",
        position: { keys: [{ t: 0, v: [540, 1000], ease: "out_expo" }, { t: 1, v: [540, 960], ease: { bezier: [0.16, 1, 0.3, 1] } }] },
        opacity: { wiggle: { frequency: 2, amplitude: 0.1, base: 0.9 } },
        axis_wdth: 100,
      },
      timing: { in: 0.5, out: 7 },
    },
    out: { op: "over", inputs: { fg: "title", bg: "bg" } },
    tag: { op: "lower_third", params: { title: "Ada" } },
  },
  components: {
    lower_third: {
      params: { title: "Title", accent: "#22d3ee" },
      nodes: {
        bar: { op: "shape", params: { kind: "rect", color: "$accent" } },
        out: { op: "text", params: { text: "$title" } },
      },
      output: "out",
    },
  },
  output: "out",
  meta: { composer: { name: "tests" }, fonts: [{ family: "Brand", src: "fonts/Brand.otf" }] },
};

// @ts-expect-error a misspelled param
export const typo: Node = { op: "text", params: { sise: 48 } };
// @ts-expect-error colors are hex or 0..1 arrays
export const named: Node = { op: "solid", params: { color: "red" } };
// @ts-expect-error not a port of `over`
export const port: Node = { op: "over", inputs: { source: "x" } };
// @ts-expect-error an enum value
export const fit: Node = { op: "image", params: { src: "a.png", fit: "stretch" } };

export function narrow(n: AnyNode) {
  if (isOp(n, "text")) {
    const p: ParamsOf<"text"> | undefined = n.params;
    return p?.font;
  }
  return undefined;
}
