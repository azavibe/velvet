export const LoadingDots = ({ className = "" }: { className?: string }) => (
  <div className={className} style={{ display: "flex", alignItems: "center", height: 14, gap: 2 }}>
    {[0, 1, 2].map((i) => (
      <div
        key={i}
        className="bg-foreground"
        style={{
          width: 3.5,
          height: 6,
          borderRadius: 2,
          transformOrigin: "center",
        }}
      />
    ))}
  </div>
);
