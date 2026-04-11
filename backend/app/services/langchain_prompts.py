"""Prompt templates and helpers for LangChain story generation.

This module contains tokenized prompt templates the generator can use.
"""

STORY_TEMPLATE = (
    "You are a creative storyteller. Compose a {tone} short story of {length} length that includes the following words: {words_list}."
    " Use natural language, ensure each word appears at least once, and keep sentences varied and engaging."
)

def build_story_prompt(words, tone='fantastic', length='short'):
    words_list = ', '.join(words)
    return STORY_TEMPLATE.format(tone=tone, length=length, words_list=words_list)
