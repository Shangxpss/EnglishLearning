import React, { useState, useEffect } from 'react'
import { WordPanel } from '../../components/WordPanel'
import { StoryViewer } from '../../components/StoryViewer'
import { useAuth } from '@/providers/auth-context'

export function ReadingPage() {
  const { user, token } = useAuth()
  const [content, setContent] = useState('')
  const [selectedWords, setSelectedWords] = useState<string[]>([])
  const [userWords, setUserWords] = useState<string[]>([])
  const [story, setStory] = useState('')
  const [sessionId, setSessionId] = useState<number | null>(null)
  const [isGenerating, setIsGenerating] = useState(false)
  const [sessionStarted, setSessionStarted] = useState(false)
  const [isLoading, setIsLoading] = useState(false)

  // Fetch user's saved words from database
  useEffect(() => {
    const fetchUserWords = async () => {
      if (token) {
        setIsLoading(true)
        try {
          const response = await fetch('/api/my-words', {
            headers: {
              'Authorization': `Bearer ${token}`
            }
          })
          const data = await response.json()
          if (data.success) {
            setUserWords(data.words.map((word: any) => word.word))
          }
        } catch (e) {
          console.error(e)
        } finally {
          setIsLoading(false)
        }
      }
    }

    fetchUserWords()
  }, [token])

  // Handle selecting words from user's saved words list
  const handleSelectUserWord = (word: string) => {
    setSelectedWords(prev => Array.from(new Set([...prev, word])))
  }

  const createSession = async () => {
    if (!token) {
      alert('Please log in to create a reading session.')
      return
    }
    try {
      const response = await fetch('/api/reading_sessions', {
        method: 'POST',
        headers: {
          'Content-Type': 'application/json',
          'Authorization': `Bearer ${token}`
        },
        body: JSON.stringify({ content })
      })
      const data = await response.json()
      setSessionId(data.reading_session_id)
      setSessionStarted(true)
    } catch (e) {
      console.error(e)
    }
  }

  const markWord = async (word: string) => {
    setSelectedWords(prev => Array.from(new Set([...prev, word])))
    try {
      if (sessionId && token) {
        await fetch(`/api/reading_sessions/${sessionId}/mark_word`, {
          method: 'POST',
          headers: {
            'Content-Type': 'application/json',
            'Authorization': `Bearer ${token}`
          },
          body: JSON.stringify({ word, snippet: '' })
        })
      } else if (!token) {
        alert('Please log in to save words.')
      } else {
        console.error('No session ID available. Please start a session first.')
      }
    } catch (e) {
      console.error(e)
    }
  }

  const generateStory = async () => {
    if (selectedWords.length === 0) {
      alert('Please select at least one word before generating a story.')
      return
    }

    setIsGenerating(true)
    try {
      const res = await fetch('/api/stories', {
        method: 'POST',
        headers: {'Content-Type': 'application/json'},
        body: JSON.stringify({ words: selectedWords, tone: 'fantastic', length: 'short', max_length: 500 })
      })
      const data = await res.json()
      setStory(data.text || '')
    } catch (e) {
      console.error(e)
    } finally {
      setIsGenerating(false)
    }
  }

  // Simple word selection by double-click for demo
  const onDoubleClick = (e: React.MouseEvent<HTMLTextAreaElement>) => {
    const selection = window.getSelection()?.toString().trim()
    if (selection) markWord(selection)
  }

  return (
    <div className="min-h-screen bg-gradient-to-br from-blue-50 to-indigo-50 py-8">
      <div className="container mx-auto px-4 max-w-7xl">
        <div className="text-center mb-10">
          <h1 className="text-4xl font-bold text-indigo-800 mb-2">Reading & Story Generation</h1>
          <p className="text-gray-600">Double-click to select words, then generate a creative story</p>
        </div>

        <div className="bg-white rounded-xl shadow-lg p-6 mb-8">
          <div className="mb-6">
            <label className="block text-sm font-medium text-gray-700 mb-2">
              Enter your text here
            </label>
            <textarea 
              value={content} 
              onChange={(e) => setContent(e.target.value)} 
              onDoubleClick={onDoubleClick} 
              rows={8} 
              className="w-full px-4 py-3 border border-gray-300 rounded-lg focus:ring-2 focus:ring-indigo-500 focus:border-indigo-500 transition-colors"
              placeholder="Paste or type text here. Double-click on words to select them."
            />
            <div className="mt-4 flex justify-end">
              <button 
                onClick={createSession}
                disabled={sessionStarted}
                className="px-6 py-2 bg-indigo-600 text-white rounded-lg hover:bg-indigo-700 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
              >
                {sessionStarted ? 'Session Started' : 'Start Session'}
              </button>
            </div>
          </div>

          <div className="grid md:grid-cols-3 gap-6">
            <div className="bg-gray-50 rounded-lg p-4">
              <h2 className="text-lg font-semibold text-gray-800 mb-4">Your Saved Words</h2>
              {isLoading ? (
                <div className="flex justify-center items-center py-8">
                  <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-gray-600"></div>
                </div>
              ) : userWords.length === 0 ? (
                <div className="text-center py-8 text-gray-500">
                  {user ? 'No saved words yet. Start saving words from the Subtitle Tool!' : 'Log in to view your saved words.'}
                </div>
              ) : (
                <div className="flex flex-wrap gap-2">
                  {userWords.map((word, index) => (
                    <button
                      key={index}
                      onClick={() => handleSelectUserWord(word)}
                      className="px-3 py-1 bg-blue-100 text-blue-800 rounded-full text-sm hover:bg-blue-200 transition-colors"
                    >
                      {word}
                    </button>
                  ))}
                </div>
              )}
            </div>

            <div className="bg-gray-50 rounded-lg p-4">
              <h2 className="text-lg font-semibold text-gray-800 mb-4">Selected Words</h2>
              <WordPanel words={selectedWords} />
            </div>

            <div className="bg-gray-50 rounded-lg p-4">
              <div className="flex justify-between items-center mb-4">
                <h2 className="text-lg font-semibold text-gray-800">Generated Story</h2>
                <button 
                  onClick={generateStory}
                  disabled={isGenerating || selectedWords.length === 0}
                  className="px-4 py-2 bg-green-600 text-white rounded-lg hover:bg-green-700 transition-colors disabled:opacity-50 disabled:cursor-not-allowed flex items-center gap-2"
                >
                  {isGenerating ? (
                    <>
                      <div className="animate-spin rounded-full h-4 w-4 border-b-2 border-white"></div>
                      Generating...
                    </>
                  ) : (
                    'Generate Story'
                  )}
                </button>
              </div>
              <StoryViewer text={story} />
            </div>
          </div>
        </div>

        <div className="bg-blue-50 border border-blue-200 rounded-lg p-4">
          <h3 className="font-medium text-blue-800 mb-2">How to use this feature:</h3>
          <ol className="list-decimal list-inside text-blue-700 space-y-1">
            <li>Paste or type text into the text area</li>
            <li>Double-click on words you want to learn</li>
            <li>Click "Start Session" to begin tracking</li>
            <li>Click "Generate Story" to create a story using your selected words</li>
          </ol>
        </div>
      </div>
    </div>
  )
}


