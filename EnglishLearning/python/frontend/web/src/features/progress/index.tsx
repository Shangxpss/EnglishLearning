import { useLoaderData } from "react-router";


export function ProgressPage() {
  // ✅ Data routing ready. Loader will be attached in router config when needed.
  const data = useLoaderData() as any | null;
  
  return (
    <div className="space-y-4">
      <h1 className="text-3xl font-bold tracking-tight">📊 Progress</h1>
      {data ? (
        <div className="grid grid-cols-2 gap-4">
          <div className="p-4 rounded-lg border bg-card text-card-foreground shadow-sm">
            <p className="text-sm text-muted-foreground">Known</p>
            <p className="text-2xl font-bold">{data.known}</p>
          </div>
          <div className="p-4 rounded-lg border bg-card text-card-foreground shadow-sm">
            <p className="text-sm text-muted-foreground">Learning</p>
            <p className="text-2xl font-bold">{data.learning}</p>
          </div>
        </div>
      ) : (
        <p className="text-muted-foreground">Loading progress...</p>
      )}
    </div>
  );
}
