/** Ephemeral prompt recall. Only accepted submissions enter the bounded history.
 * Browsing preserves the unsent draft; editing a recalled prompt exits browsing. */
export class PromptHistory {
  private entries: string[] = [];
  private cursor: number | null = null;
  private draft = '';
  accept(text: string) {
    if (this.entries[this.entries.length - 1] !== text) this.entries.push(text);
    this.entries = this.entries.slice(-64);
    this.reset();
  }
  get browsing() {
    return this.cursor !== null;
  }
  restore() {
    const draft = this.draft;
    this.reset();
    return draft;
  }
  reset() {
    this.cursor = null;
    this.draft = '';
  }
  previous(current: string): string | null {
    if (!this.entries.length || (this.cursor === null && current !== '')) return null;
    if (this.cursor === null) {
      this.draft = current;
      this.cursor = this.entries.length;
    }
    this.cursor = Math.max(0, this.cursor - 1);
    return this.entries[this.cursor] ?? null;
  }
  next(): string | null {
    if (this.cursor === null) return null;
    this.cursor += 1;
    if (this.cursor >= this.entries.length) {
      const draft = this.draft;
      this.reset();
      return draft;
    }
    return this.entries[this.cursor] ?? null;
  }
}
