import { useEffect, useState } from "react";
import { cachedCaseImageSrc } from "../lib/caseImage";

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

  useEffect(() => {
    let cancelled = false;
    setFailed(false);
    setSrc(null);
    void cachedCaseImageSrc(image).then((value) => {
      if (!cancelled) setSrc(value);
    });
    return () => {
      cancelled = true;
    };
  }, [image]);

  if (failed || !src) {
    return <div className={`flex h-full items-center justify-center bg-slate-800 px-2 text-center text-[10px] text-slate-500 ${className ?? ""}`}>{failed ? alt : "加载中…"}</div>;
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
