export interface Message {
  id: string;
  role: "user" | "assistant";
  content: string;
}

export interface VocabularyWord {
  id: string;
  word: string;
  status: "unknown" | "learning" | "known";
  strength: number;
}

export interface VocabularyProgress {
  total: number;
  known: number;
  learning: number;
  unknown: number;
}
