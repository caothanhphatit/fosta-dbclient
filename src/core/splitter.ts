export function makeSplitter(handle: HTMLElement, axis: "x" | "y", onMove: (delta: number) => void): void {
  const coordinate = (event: MouseEvent) => (axis === "x" ? event.clientX : event.clientY);
  handle.addEventListener("mousedown", (start) => {
    start.preventDefault();
    let last = coordinate(start);
    const move = (event: MouseEvent) => {
      const current = coordinate(event);
      onMove(current - last);
      last = current;
    };
    const release = () => {
      document.removeEventListener("mousemove", move);
      document.removeEventListener("mouseup", release);
      document.body.classList.remove(`resizing-${axis}`);
    };
    document.body.classList.add(`resizing-${axis}`);
    document.addEventListener("mousemove", move);
    document.addEventListener("mouseup", release);
  });
}
