export function HomePage() {
  return (
    <div className="space-y-6">
      <div className="text-center space-y-4">
        <h1 className="text-4xl font-bold tracking-tight">Welcome to EnglishPro</h1>
        <p className="text-muted-foreground">Your personal English learning assistant</p>
      </div>
      
      <div className="grid md:grid-cols-3 gap-6">
        <div className="bg-card p-6 rounded-lg border">
          <h2 className="text-xl font-semibold mb-2">Learn with Audio</h2>
          <p className="text-muted-foreground">Improve your listening and pronunciation skills</p>
        </div>
        <div className="bg-card p-6 rounded-lg border">
          <h2 className="text-xl font-semibold mb-2">Subtitle Practice</h2>
          <p className="text-muted-foreground">Enhance your reading and comprehension</p>
        </div>
        <div className="bg-card p-6 rounded-lg border">
          <h2 className="text-xl font-semibold mb-2">Track Progress</h2>
          <p className="text-muted-foreground">Monitor your learning journey</p>
        </div>
      </div>
    </div>
  );
}