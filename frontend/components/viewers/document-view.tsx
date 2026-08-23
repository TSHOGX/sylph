"use client";

/**
 * Word documents and PowerPoint decks.
 *
 * One renderer for both, which is the payoff of the shared block IR: a slide is a
 * section whose blocks follow it, so the only difference at this level is whether
 * sections exist and how they are framed.
 *
 * Sections are kept rather than flattened into headings because a titleless
 * slide — image-only, or a continuation — would otherwise merge into the slide
 * above it, inferring structure that was deliberately discarded.
 *
 * No new dependency: the payload is already structured, so this is presentation
 * over JSON. That is what the block-list decision on the backend bought.
 */

import { useMemo } from "react";
import type { BlocksPayload, DocumentBlock, DocumentSection } from "@/lib/api";

export function DocumentView({
  payload,
  /** Slides get numbered cards; a Word document is one continuous flow. */
  slides,
}: {
  payload: BlocksPayload;
  slides: boolean;
}) {
  // Blocks are flat with sections pointing into them, so grouping happens here
  // rather than on the wire — the client is where the framing decision lives.
  const grouped = useMemo(() => groupBySection(payload), [payload]);

  return (
    <div className="min-h-0 flex-1 overflow-auto overscroll-contain px-3 py-2">
      {payload.blocks.length === 0 && payload.sections.length === 0 && (
        <p className="text-2xs text-ink-muted">
          This document holds no readable text. It may be made up entirely of
          images or shapes.
        </p>
      )}

      {grouped.map(({ section, blocks }, index) =>
        section && slides ? (
          <section
            key={section.index}
            aria-label={section.title ?? `Slide ${section.index + 1}`}
            className="mb-3 rounded border border-line bg-raised"
          >
            <header className="flex items-baseline gap-2 border-b border-line bg-sunken px-2.5 py-1">
              <span className="shrink-0 font-mono text-3xs text-ink-muted">
                {section.index + 1}
              </span>
              <h4 className="min-w-0 flex-1 truncate text-xs font-semibold text-ink">
                {section.title ?? (
                  <span className="font-normal italic text-ink-muted">Untitled slide</span>
                )}
              </h4>
            </header>
            <div className="px-3 py-2">
              <Blocks blocks={blocks} />
              {section.notes && (
                // Notes are frequently where a deck's actual argument lives, so
                // they are shown rather than dropped — but set apart, because
                // they are not what the slide displays.
                <aside className="mt-2 border-t border-dashed border-line pt-1.5">
                  <p className="text-3xs font-medium uppercase tracking-wide text-ink-muted">
                    Speaker notes
                  </p>
                  <p className="mt-0.5 text-xs leading-relaxed text-ink-muted">
                    {section.notes}
                  </p>
                </aside>
              )}
            </div>
          </section>
        ) : (
          <div key={section?.index ?? `flow-${index}`} className="text-ink">
            <Blocks blocks={blocks} />
          </div>
        ),
      )}

      {payload.truncated && (
        <p className="mt-2 rounded border border-warning-line bg-warning-bg px-2 py-1 text-3xs text-warning">
          This document is longer than the preview shows.
        </p>
      )}
    </div>
  );
}

function Blocks({ blocks }: { blocks: DocumentBlock[] }) {
  return (
    <>
      {blocks.map((block, index) => (
        <Block key={index} block={block} />
      ))}
    </>
  );
}

function Block({ block }: { block: DocumentBlock }) {
  switch (block.type) {
    case "heading":
      return <Heading level={block.level} text={block.text} />;

    case "paragraph":
      return (
        <p className="mb-1.5 text-xs leading-relaxed">
          {block.spans.map((span, index) => {
            // Nested rather than combined so bold-italic renders as both, which
            // a single className ternary would get wrong.
            let node = <>{span.text}</>;
            if (span.italic) node = <em>{node}</em>;
            if (span.bold) node = <strong className="font-semibold">{node}</strong>;
            return <span key={index}>{node}</span>;
          })}
        </p>
      );

    case "listItem":
      return (
        // A flat row with an indent rather than real nested `<ul>`s: the payload
        // is a flat list with levels, and reconstructing nesting would mean
        // guessing where each list starts and ends.
        <div
          className="mb-0.5 flex gap-1.5 text-xs leading-relaxed"
          style={{ paddingLeft: block.level * 14 }}
        >
          <span aria-hidden="true" className="shrink-0 text-ink-faint">
            {block.ordered ? "·" : "–"}
          </span>
          <span className="min-w-0">{block.text}</span>
        </div>
      );

    case "table":
      return (
        <div className="mb-2 overflow-x-auto">
          <table className="border-collapse text-xs">
            <tbody>
              {block.rows.map((row, rowIndex) => (
                <tr key={rowIndex} className={rowIndex === 0 ? "bg-sunken" : undefined}>
                  {row.map((cell, cellIndex) =>
                    rowIndex === 0 ? (
                      <th
                        key={cellIndex}
                        scope="col"
                        className="border border-line px-2 py-0.5 text-left font-medium text-ink"
                      >
                        {cell}
                      </th>
                    ) : (
                      <td
                        key={cellIndex}
                        className="border border-line px-2 py-0.5 text-ink"
                      >
                        {cell}
                      </td>
                    ),
                  )}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      );
  }
}

/**
 * A document heading, offset to start at `h4`.
 *
 * The page owns `h1`–`h3`, so a document's own `h1` rendered literally would
 * break the page outline a screen-reader user navigates by. Same offset the
 * Markdown renderer already applies, for the same reason.
 */
function Heading({ level, text }: { level: number; text: string }) {
  const clamped = Math.min(Math.max(level, 1), 3);
  const className = [
    "mb-1 mt-2 font-semibold text-ink",
    clamped === 1 ? "text-base" : clamped === 2 ? "text-sm" : "text-xs",
  ].join(" ");

  if (clamped === 1) return <h4 className={className}>{text}</h4>;
  if (clamped === 2) return <h5 className={className}>{text}</h5>;
  return <h6 className={className}>{text}</h6>;
}

/**
 * Split the flat block list at section boundaries.
 *
 * Any blocks before the first section stay in a leading group with no section, so
 * content that precedes slide one is shown rather than silently dropped.
 */
function groupBySection(
  payload: BlocksPayload,
): { section: DocumentSection | null; blocks: DocumentBlock[] }[] {
  if (payload.sections.length === 0) {
    return [{ section: null, blocks: payload.blocks }];
  }

  const groups: { section: DocumentSection | null; blocks: DocumentBlock[] }[] = [];
  const first = payload.sections[0].firstBlock;
  if (first > 0) {
    groups.push({ section: null, blocks: payload.blocks.slice(0, first) });
  }

  payload.sections.forEach((section, index) => {
    const end = payload.sections[index + 1]?.firstBlock ?? payload.blocks.length;
    groups.push({
      section,
      blocks: payload.blocks.slice(section.firstBlock, end),
    });
  });

  return groups;
}
