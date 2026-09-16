import React from 'react';

interface WordCardProps {
  word: string;
  score: number | null;
  hoverScore: number | null;
  index: number;
  onScoreChange: (index: number, score: number) => void;
  onHoverStart: (index: number, score: number) => void;
  onHoverEnd: (index: number) => void;
  onSaveWord: (word: string, score: number) => void;
}

const getScoreLabel = (score: number | null) => {
  if (score === null) return 'Not rated';
  switch (score) {
    case 1: return 'Unfamiliar';
    case 2: return 'Somewhat familiar';
    case 3: return 'Familiar';
    case 4: return 'Very familiar';
    case 5: return 'Expert';
    default: return 'Not rated';
  }
};

export const WordCard: React.FC<WordCardProps> = ({
  word,
  score,
  hoverScore,
  index,
  onScoreChange,
  onHoverStart,
  onHoverEnd,
  onSaveWord
}) => {
  return (
    <div className="p-4 border rounded-md space-y-2">
      <div className="flex justify-between items-center">
        <span className="text-lg font-medium">{word}</span>
        <span className="text-sm text-muted-foreground">
          {getScoreLabel(score)}
        </span>
      </div>
      <div className="flex space-x-1" onMouseLeave={() => onHoverEnd(index)}>
        {[1, 2, 3, 4, 5].map((starScore) => (
          <button
            key={starScore}
            onClick={() => onScoreChange(index, starScore)}
            onMouseEnter={() => onHoverStart(index, starScore)}
            className={`p-1 rounded-full ${(hoverScore ?? score ?? 0) >= starScore ? 'text-yellow-400' : 'text-gray-300'} hover:text-yellow-300 transition-colors`}
            aria-label={`Rate ${word} as ${getScoreLabel(starScore)}`}
          >
            <svg xmlns="http://www.w3.org/2000/svg" className="h-5 w-5" fill="currentColor" viewBox="0 0 20 20">
              <path d="M9.049 2.927c.3-.921 1.603-.921 1.902 0l1.07 3.292a1 1 0 00.95.69h3.462c.969 0 1.371 1.24.588 1.81l-2.8 2.034a1 1 0 00-.364 1.118l1.07 3.292c.3.921-.755 1.688-1.54 1.118l-2.8-2.034a1 1 0 00-1.175 0l-2.8 2.034c-.784.57-1.838-.197-1.539-1.118l1.07-3.292a1 1 0 00-.364-1.118L2.98 8.72c-.783-.57-.38-1.81.588-1.81h3.461a1 1 0 00.951-.69l1.07-3.292z" />
            </svg>
          </button>
        ))}
      </div>
      <button
        onClick={() => score && onSaveWord(word, score)}
        disabled={!score}
        className="w-full mt-2 py-1 px-3 bg-blue-500 text-white rounded-md text-sm hover:bg-blue-600 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
      >
        Save Word
      </button>
    </div>
  );
};