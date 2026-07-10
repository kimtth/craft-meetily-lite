import { useEffect, useRef, useState } from 'react';
import { cancelAreaSelection, completeAreaSelection } from '../nativeClient';

type Point = { x: number; y: number };

export function AreaSelector() {
  const surfaceRef = useRef<HTMLDivElement>(null);
  const dragPointerId = useRef<number | null>(null);
  const [start, setStart] = useState<Point | null>(null);
  const [end, setEnd] = useState<Point | null>(null);
  const selection = start && end ? { x: Math.min(start.x, end.x), y: Math.min(start.y, end.y), width: Math.abs(end.x - start.x), height: Math.abs(end.y - start.y) } : null;

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        void cancelAreaSelection();
      }
    };
    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, []);

  const pointFromEvent = (event: React.PointerEvent<HTMLDivElement>): Point => {
    const bounds = surfaceRef.current!.getBoundingClientRect();
    return {
      x: Math.min(Math.max(event.clientX - bounds.left, 0), bounds.width),
      y: Math.min(Math.max(event.clientY - bounds.top, 0), bounds.height),
    };
  };

  const completeSelection = async (startPoint: Point, endPoint: Point) => {
    const bounds = surfaceRef.current?.getBoundingClientRect();
    if (!bounds) return;
    const nextSelection = {
      x: Math.min(startPoint.x, endPoint.x),
      y: Math.min(startPoint.y, endPoint.y),
      width: Math.abs(endPoint.x - startPoint.x),
      height: Math.abs(endPoint.y - startPoint.y),
    };
    if (nextSelection.width < 12 || nextSelection.height < 12) return;
    await completeAreaSelection({
      ...nextSelection,
      viewportWidth: bounds.width,
      viewportHeight: bounds.height,
    });
  };

  return (
    <main className="area-selector" aria-label="Select recording area">
      <div
        ref={surfaceRef}
        className="area-selector-surface"
        onPointerDown={(event) => {
          if (!event.isPrimary || event.button !== 0) return;
          dragPointerId.current = event.pointerId;
          try {
            event.currentTarget.setPointerCapture(event.pointerId);
          } catch {
            // Pointer capture is unavailable in some embedded webviews; the active pointer is tracked separately.
          }
          const point = pointFromEvent(event);
          setStart(point);
          setEnd(point);
        }}
        onPointerMove={(event) => start && dragPointerId.current === event.pointerId && setEnd(pointFromEvent(event))}
        onPointerUp={(event) => {
          if (!start || dragPointerId.current !== event.pointerId) return;
          dragPointerId.current = null;
          if (event.currentTarget.hasPointerCapture(event.pointerId)) {
            event.currentTarget.releasePointerCapture(event.pointerId);
          }
          const endPoint = pointFromEvent(event);
          setEnd(endPoint);
          void completeSelection(start, endPoint);
        }}
        onPointerCancel={() => {
          dragPointerId.current = null;
          setStart(null);
          setEnd(null);
        }}
        onContextMenu={(event) => {
          event.preventDefault();
          void cancelAreaSelection();
        }}
      >
        {selection && <div className="area-selector-box" style={{ left: selection.x, top: selection.y, width: selection.width, height: selection.height }}>
          {selection.width >= 12 && selection.height >= 12 && <span>{Math.round(selection.width)} × {Math.round(selection.height)}</span>}
        </div>}
      </div>
    </main>
  );
}