import React, { useState } from 'react'
import { WordPanel } from '../components/WordPanel'
import { StoryViewer } from '../components/StoryViewer'

const ReadingPage: React.FC = () => {
  const [content, setContent] = useState('')
  const [selectedWords, setSelectedWords] = useState<string[]>([])
  const [story, setStory] = useState('')
  const [sessionId, setSessionId] = useState<number | null>(null)

  const createSession = async () => {
    try {
      const response = await fetch('/reading_sessions', {
        method: 'POST',
        headers: {'Content-Type': 'application/json'},
        body: JSON.stringify({ content })
      })
      const data = await response.json()
      setSessionId(data.id)
    } catch (e) {
      console.error(e)
    }
  }

  const markWord = async (word: string) => {
    setSelectedWords(prev => Array.from(new Set([...prev, word])))
    try {
      if (sessionId) {
        await fetch(`/reading_sessions/${sessionId}/mark_word`, {
          method: 'POST',
          headers: {'Content-Type': 'application/json'},
          body: JSON.stringify({ word, snippet: '' })
        })
      } else {
        console.error('No session ID available. Please start a session first.')
      }
    } catch (e) {
      console.error(e)
    }
  }

  const generateStory = async () => {
    try {
      const res = await fetch('/stories', {
        method: 'POST',
        headers: {'Content-Type': 'application/json'},
        body: JSON.stringify({ words: selectedWords, tone: 'fantastic', length: 'short' })
      })
      const data = await res.json()
      setStory(data.text || '')
    } catch (e) {
      console.error(e)
    }
  }

  // Simple word selection by double-click for demo
  const onDoubleClick = (e: React.MouseEvent<HTMLTextAreaElement>) => {
    const selection = window.getSelection()?.toString().trim()
    if (selection) markWord(selection)
  }

  return (
    <div>
      <h1>Reading</h1>
      <button onClick={createSession}>Start Session</button>
      <textarea value={content} onChange={(e) => setContent(e.target.value)} onDoubleClick={onDoubleClick} rows={10} cols={80} />
      <div style={{display: 'flex', gap: 20}}>
        <WordPanel words={selectedWords} />
        <div>
          <button onClick={generateStory}>Generate Story</button>
          <StoryViewer text={story} />
        </div>
      </div>
    </div>
  )
}

export default ReadingPage
