import { useState, useCallback } from "react";
import { Message } from "../types";

export function useChat(apiUrl = "http://localhost:8000/api/chat") {
  const [messages, setMessages] = useState<Message[]>([]);
  const [input, setInput] = useState("");
  const [isLoading, setIsLoading] = useState(false);

  const handleSubmit = useCallback(
    async (e: React.FormEvent) => {
      e.preventDefault();
      if (!input.trim() || isLoading) return;

      const userMsg: Message = {
        id: Date.now().toString(),
        role: "user",
        content: input,
      };
      setMessages((prev) => [...prev, userMsg]);
      setInput("");
      setIsLoading(true);

      try {
        const res = await fetch(apiUrl, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ message: input, user_id: "user-123" }),
        });
        const data = await res.json();
        const assistantMsg: Message = {
          id: (Date.now() + 1).toString(),
          role: "assistant",
          content: data.response,
        };
        setMessages((prev) => [...prev, assistantMsg]);
      } catch (err) {
        console.error(err);
      } finally {
        setIsLoading(false);
      }
    },
    [input, isLoading, apiUrl],
  );

  return { messages, input, setInput, handleSubmit, isLoading };
}
