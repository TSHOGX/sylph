/**
 * Incremental Server-Sent Events frame decoder.
 *
 * # Why this exists rather than `EventSource`
 *
 * `EventSource` dispatches a frame to the listener registered for its `event:`
 * name, and the backend names every frame after the event kind (`event:
 * thinking`, `event: tool_call`, ...). That design has three failure modes, and
 * this project shipped the first one:
 *
 * 1. `onmessage` fires only for frames named `message`, so a client that listens
 *    there receives *nothing* from a stream of named frames. The turn appeared to
 *    arrive in one lump at the end because the only thing repainting the
 *    transcript was the history refetch on completion.
 * 2. A frame named `error` is dispatched as an `error` event on the `EventSource`
 *    object, which is the same event type the browser uses for connection
 *    failures. Any `addEventListener("error", ...)` conflates an agent error with
 *    a dropped socket.
 * 3. Registering one listener per known kind means a kind the backend adds later
 *    is silently dropped. Silent event loss is the worst failure mode available
 *    here — it looks exactly like an agent that did nothing.
 *
 * Parsing the stream ourselves removes all three: the frame name is read but
 * never used for dispatch, so every frame reaches the consumer regardless of what
 * it is called, and a new kind needs no client change.
 *
 * The parser is also the reason this file exists separately from the hook. It is
 * pure and synchronous, so it is unit-testable in Vitest's `node` environment —
 * `EventSource` is not, which is precisely why the original bug had no test.
 *
 * # Conformance
 *
 * Follows the WHATWG event-stream parsing rules: `\n`, `\r\n`, and `\r` all
 * terminate a line; a blank line dispatches; a line beginning with `:` is a
 * comment (this is what keep-alives are); a field with no colon has an empty
 * value; one optional leading space after the colon is stripped; `data:` lines
 * accumulate and are joined with newlines. A frame carrying no `data` is not
 * dispatched, which is what makes keep-alive comments free.
 */

/** One dispatched event-stream frame. */
export interface SSEFrame {
  /** The `event:` field, or `"message"` when the frame did not name one. */
  event: string;
  /** `data:` lines joined with `\n`. */
  data: string;
  /** The `id:` field, when the frame carried one. */
  id?: string;
  /** `retry:` in milliseconds, when the frame carried a valid one. */
  retry?: number;
}

export interface SSEDecoder {
  /**
   * Feed the next chunk of decoded text and take whatever frames it completed.
   *
   * A chunk may split a line, a frame, or a multi-byte character mid-way; the
   * decoder keeps the remainder and resumes on the next call.
   */
  push(chunk: string): SSEFrame[];
}

export function createSSEDecoder(): SSEDecoder {
  /** Bytes seen but not yet forming a complete line. */
  let pending = "";
  let dataLines: string[] = [];
  let eventName = "";
  let lastId: string | undefined;
  let retry: number | undefined;

  function reset() {
    dataLines = [];
    eventName = "";
    retry = undefined;
  }

  function takeLine(): string | null {
    const lf = pending.indexOf("\n");
    const cr = pending.indexOf("\r");
    if (lf === -1 && cr === -1) return null;

    // A lone trailing `\r` is ambiguous: the `\n` of a `\r\n` pair may be in the
    // next chunk. Waiting avoids splitting one terminator into two empty lines,
    // which would dispatch a frame early.
    if (cr !== -1 && (lf === -1 || cr < lf)) {
      if (cr === pending.length - 1) return null;
      const line = pending.slice(0, cr);
      pending = pending.slice(pending[cr + 1] === "\n" ? cr + 2 : cr + 1);
      return line;
    }

    const line = pending.slice(0, lf);
    pending = pending.slice(lf + 1);
    return line;
  }

  return {
    push(chunk: string): SSEFrame[] {
      pending += chunk;
      const frames: SSEFrame[] = [];

      for (let line = takeLine(); line !== null; line = takeLine()) {
        // Blank line: dispatch. A frame with no data carries nothing to render,
        // so it is dropped rather than surfaced as an empty event.
        if (line === "") {
          if (dataLines.length > 0) {
            frames.push({
              event: eventName || "message",
              data: dataLines.join("\n"),
              ...(lastId !== undefined && { id: lastId }),
              ...(retry !== undefined && { retry }),
            });
          }
          reset();
          continue;
        }

        if (line.startsWith(":")) continue; // comment / keep-alive

        const colon = line.indexOf(":");
        const field = colon === -1 ? line : line.slice(0, colon);
        let value = colon === -1 ? "" : line.slice(colon + 1);
        if (value.startsWith(" ")) value = value.slice(1);

        switch (field) {
          case "event":
            eventName = value;
            break;
          case "data":
            dataLines.push(value);
            break;
          case "id":
            // The spec ignores an id containing NUL; nothing else is rejected.
            if (!value.includes("\0")) lastId = value;
            break;
          case "retry": {
            // Digits only, per spec: a malformed value is ignored rather than
            // collapsing the reconnect delay to NaN.
            if (/^\d+$/.test(value)) retry = Number(value);
            break;
          }
          default:
            break; // Unknown fields are ignored, which is what makes SSE extensible.
        }
      }

      return frames;
    },
  };
}
