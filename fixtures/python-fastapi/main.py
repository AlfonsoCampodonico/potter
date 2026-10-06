"""Entrypoint so ``uvicorn main:app`` (a common auto-detected default)
keeps working. The application itself lives in the ``app`` package — see
``app/main.py``.
"""

from app.main import app

__all__ = ["app"]
