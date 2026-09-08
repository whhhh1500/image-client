import { useEffect, useRef, useState } from "react";
import { cachedCaseImageSrc } from "../lib/caseImage";

/**
 * Case-library thumbnail that only downloads once it approaches the viewport.
 * The library holds 500+ images (~150 MB), and a plain mount effect used to
 * start every download as soon as the grid rendered.
 */
export default function CachedCaseImage({
  image,
  alt,
  className,
}: {
  image: string;
  alt: string;
  className?: string;
}) {
  const [src, setSrc] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  const [visible, setVisible] = useState(false);
  const holderRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    const node = holderRef.current;
    if (!node) return;
    if (typeof IntersectionObserver === "undefined") {
      setVisible(true);
      return;
    }
    const observer = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) {
        setVisible(true);
        observer.disconnect();
      }
    }, { rootMargin: "300px" });
    observer.observe(node);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    if (!visible) return;
    let cancelled = false;
    setFailed(false);
    setSrc(null);
    void cachedCaseImageSrc(image)
      .then((value) => {
        if (!cancelled) setSrc(value);
      })
      .catch(() => {
        if (!cancelled) setFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, [image, visible]);

  if (failed || !src) {
    return <div ref={holderRef} className={`flex h-full items-center justify-center bg-slate-800 px-2 text-center text-[10px] text-slate-500 ${className ?? ""}`}>{failed ? alt : "加载中…"}</div>;
  }

  return (
    <img
      src={src}
      alt={alt}
      loading="lazy"
      className={className}
      onError={() => setFailed(true)}
    />
  );
}
