import { useState } from 'react';
import { WordCard } from '../components/WordCard';

interface WordScore {
  word: string;
  score: number | null;
  hoverScore: number | null;
}

export function SubtitlePage() {
  const [file, setFile] = useState<File | null>(null);
  const [words, setWords] = useState<WordScore[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [uploadType, setUploadType] = useState<'subtitle' | 'audio'>('subtitle');

  const handleFileChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    if (e.target.files && e.target.files[0]) {
      setFile(e.target.files[0]);
      setError(null);
    }
  };

  const handleUpload = async () => {
    if (!file) {
      setError('Please select a file');
      return;
    }

    setLoading(true);
    setError(null);

    try {
      const formData = new FormData();
      formData.append('file', file);

      let endpoint = '';
      if (uploadType === 'subtitle') {
        endpoint = '/api/upload-subtitle/';
      } else {
        endpoint = '/api/process-audio-with-subtitles/';
      }

      const response = await fetch(endpoint, {
        method: 'POST',
        body: formData,
      });

      const data = await response.json();

      if (data.success) {
        let extractedWords: string[] = [];
        if (uploadType === 'subtitle') {
          extractedWords = data.words;
        } else {
          extractedWords = data.data.keywords;
        }

        const wordScores: WordScore[] = extractedWords.map((word: string) => ({
          word,
          score: null,
          hoverScore: null
        }));
        setWords(wordScores);
      } else {
        setError(data.message || 'Failed to process file');
      }
    } catch (err) {
      setError('An error occurred while uploading the file');
    } finally {
      setLoading(false);
    }
  };

  const handleScoreChange = (index: number, score: number) => {
    const updatedWords = [...words];
    updatedWords[index].score = score;
    updatedWords[index].hoverScore = null;
    setWords(updatedWords);
  };

  const handleHoverStart = (index: number, score: number) => {
    const updatedWords = [...words];
    updatedWords[index].hoverScore = score;
    setWords(updatedWords);
  };

  const handleHoverEnd = (index: number) => {
    const updatedWords = [...words];
    updatedWords[index].hoverScore = null;
    setWords(updatedWords);
  };

  return (
    <div className="space-y-6">
      <h1 className="text-3xl font-bold tracking-tight">📺 Subtitle Analyzer</h1>
      <p className="text-muted-foreground">Upload .srt/.txt files or audio files to extract vocabulary and rate your familiarity.</p>

      <div className="space-y-4">
        <div className="flex items-center space-x-4">
          <div className="flex space-x-4">
            <label className="flex items-center space-x-2">
              <input
                type="radio"
                name="uploadType"
                value="subtitle"
                checked={uploadType === 'subtitle'}
                onChange={() => setUploadType('subtitle')}
              />
              <span>Subtitle File</span>
            </label>
            <label className="flex items-center space-x-2">
              <input
                type="radio"
                name="uploadType"
                value="audio"
                checked={uploadType === 'audio'}
                onChange={() => setUploadType('audio')}
              />
              <span>Audio File</span>
            </label>
          </div>
        </div>

        <div className="flex items-center space-x-4">
          <input
            type="file"
            accept={uploadType === 'subtitle' ? '.srt,.txt' : 'audio/*'}
            onChange={handleFileChange}
            className="file-input file-input-bordered w-full max-w-xs"
          />
          <button
            onClick={handleUpload}
            disabled={loading || !file}
            className="btn btn-primary"
          >
            {loading ? 'Processing...' : 'Upload'}
          </button>
        </div>

        {error && (
          <div className="alert alert-error">
            <svg xmlns="http://www.w3.org/2000/svg" className="h-6 w-6 shrink-0 stroke-current" fill="none" viewBox="0 0 24 24">
              <path strokeLinecap="round" strokeLinejoin="round" strokeWidth="2" d="M12 9v2m0 4h.01m-6.938 4h13.856c1.54 0 2.502-1.667 1.732-2.5L13.732 4c-.77-.833-2.342-.833-3.11 0L3.732 16.5c-.77.833.192 2.5 1.732 2.5z" />
            </svg>
            <span>{error}</span>
          </div>
        )}

        {words.length > 0 && (
          <div className="space-y-4">
            <h2 className="text-xl font-semibold">Extracted Words</h2>
            <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
              {words.map((wordScore, index) => (
                <WordCard
                  key={index}
                  word={wordScore.word}
                  score={wordScore.score}
                  hoverScore={wordScore.hoverScore}
                  index={index}
                  onScoreChange={handleScoreChange}
                  onHoverStart={handleHoverStart}
                  onHoverEnd={handleHoverEnd}
                />
              ))}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}