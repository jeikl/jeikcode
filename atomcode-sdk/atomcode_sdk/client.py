"""HTTP client for AtomCode serve compatible APIs."""

from __future__ import annotations

from typing import Any, AsyncIterator, Iterator, Optional, Union

import httpx

from .events import ReasoningEffort, StreamEvent, TurnResult, collect_events
from .stream import Protocol, StreamParser


MessageContent = Union[str, list[dict[str, Any]]]
Message = dict[str, Any]


def _chat_body(
    *,
    messages: list[Message],
    model: Optional[str],
    user: Optional[str],
    system: Optional[str],
    stream: bool,
    extra_body: Optional[dict[str, Any]],
) -> dict[str, Any]:
    msgs = list(messages)
    if system:
        msgs = [{"role": "system", "content": system}, *msgs]
    body: dict[str, Any] = {"messages": msgs, "stream": stream}
    if model:
        body["model"] = model
    if user:
        body["user"] = user
    if extra_body:
        body.update(extra_body)
    return body


class _ChatAPI:
    def __init__(self, client: "AtomCodeClient") -> None:
        self._c = client

    def stream(
        self,
        *,
        messages: list[Message],
        model: Optional[str] = None,
        user: Optional[str] = None,
        system: Optional[str] = None,
        extra_body: Optional[dict[str, Any]] = None,
        include_tool_output_in_reasoning: bool = False,
        reasoning_effort: ReasoningEffort | str = ReasoningEffort.MEDIUM,
    ) -> Iterator[StreamEvent]:
        """``POST /v1/chat/completions`` with ``stream=true``.

        ``reasoning_effort``: ``low`` | ``medium`` (default) | ``max``.
        See :class:`~atomcode_sdk.events.ReasoningEffort`.
        """
        body = _chat_body(
            messages=messages,
            model=model,
            user=user,
            system=system,
            stream=True,
            extra_body=extra_body,
        )
        return self._c._stream(
            "POST",
            "/v1/chat/completions",
            json=body,
            protocol=Protocol.CHAT_COMPLETIONS,
            include_tool_output_in_reasoning=include_tool_output_in_reasoning,
            reasoning_effort=reasoning_effort,
        )

    def run(
        self,
        *,
        messages: list[Message],
        model: Optional[str] = None,
        user: Optional[str] = None,
        system: Optional[str] = None,
        extra_body: Optional[dict[str, Any]] = None,
        include_tool_output_in_reasoning: bool = False,
        reasoning_effort: ReasoningEffort | str = ReasoningEffort.MEDIUM,
    ) -> TurnResult:
        """Stream a full turn and return aggregated ``reasoning`` + ``content``."""
        return collect_events(
            self.stream(
                messages=messages,
                model=model,
                user=user,
                system=system,
                extra_body=extra_body,
                include_tool_output_in_reasoning=include_tool_output_in_reasoning,
                reasoning_effort=reasoning_effort,
            )
        )

    def create(
        self,
        *,
        messages: list[Message],
        model: Optional[str] = None,
        user: Optional[str] = None,
        system: Optional[str] = None,
        stream: bool = False,
        extra_body: Optional[dict[str, Any]] = None,
        include_tool_output_in_reasoning: bool = False,
        reasoning_effort: ReasoningEffort | str = ReasoningEffort.MEDIUM,
    ) -> Union[Iterator[StreamEvent], dict[str, Any]]:
        if stream:
            return self.stream(
                messages=messages,
                model=model,
                user=user,
                system=system,
                extra_body=extra_body,
                include_tool_output_in_reasoning=include_tool_output_in_reasoning,
                reasoning_effort=reasoning_effort,
            )
        body = _chat_body(
            messages=messages,
            model=model,
            user=user,
            system=system,
            stream=False,
            extra_body=extra_body,
        )
        return self._c._request_json("POST", "/v1/chat/completions", json=body)


class _ResponsesAPI:
    def __init__(self, client: "AtomCodeClient") -> None:
        self._c = client

    def stream(
        self,
        *,
        input: Optional[Union[str, list[dict[str, Any]]]] = None,
        messages: Optional[list[Message]] = None,
        model: Optional[str] = None,
        user: Optional[str] = None,
        instructions: Optional[str] = None,
        extra_body: Optional[dict[str, Any]] = None,
        include_tool_output_in_reasoning: bool = False,
        reasoning_effort: ReasoningEffort | str = ReasoningEffort.MEDIUM,
    ) -> Iterator[StreamEvent]:
        body: dict[str, Any] = {"stream": True}
        if input is not None:
            body["input"] = input
        if messages is not None:
            body["messages"] = messages
        if model:
            body["model"] = model
        if user:
            body["user"] = user
        if instructions:
            body["instructions"] = instructions
        if extra_body:
            body.update(extra_body)
        return self._c._stream(
            "POST",
            "/v1/responses",
            json=body,
            protocol=Protocol.RESPONSES,
            include_tool_output_in_reasoning=include_tool_output_in_reasoning,
            reasoning_effort=reasoning_effort,
        )

    def run(
        self,
        *,
        input: Optional[Union[str, list[dict[str, Any]]]] = None,
        messages: Optional[list[Message]] = None,
        model: Optional[str] = None,
        user: Optional[str] = None,
        instructions: Optional[str] = None,
        extra_body: Optional[dict[str, Any]] = None,
        include_tool_output_in_reasoning: bool = False,
        reasoning_effort: ReasoningEffort | str = ReasoningEffort.MEDIUM,
    ) -> TurnResult:
        return collect_events(
            self.stream(
                input=input,
                messages=messages,
                model=model,
                user=user,
                instructions=instructions,
                extra_body=extra_body,
                include_tool_output_in_reasoning=include_tool_output_in_reasoning,
                reasoning_effort=reasoning_effort,
            )
        )


class _MessagesAPI:
    def __init__(self, client: "AtomCodeClient") -> None:
        self._c = client

    def stream(
        self,
        *,
        messages: list[Message],
        model: Optional[str] = None,
        user: Optional[str] = None,
        system: Optional[Union[str, list[dict[str, Any]]]] = None,
        extra_body: Optional[dict[str, Any]] = None,
        include_tool_output_in_reasoning: bool = False,
        reasoning_effort: ReasoningEffort | str = ReasoningEffort.MEDIUM,
    ) -> Iterator[StreamEvent]:
        body: dict[str, Any] = {"messages": messages, "stream": True}
        if model:
            body["model"] = model
        if user:
            body["user"] = user
        if system is not None:
            body["system"] = system
        if extra_body:
            body.update(extra_body)
        return self._c._stream(
            "POST",
            "/v1/messages",
            json=body,
            protocol=Protocol.MESSAGES,
            include_tool_output_in_reasoning=include_tool_output_in_reasoning,
            reasoning_effort=reasoning_effort,
        )

    def run(
        self,
        *,
        messages: list[Message],
        model: Optional[str] = None,
        user: Optional[str] = None,
        system: Optional[Union[str, list[dict[str, Any]]]] = None,
        extra_body: Optional[dict[str, Any]] = None,
        include_tool_output_in_reasoning: bool = False,
        reasoning_effort: ReasoningEffort | str = ReasoningEffort.MEDIUM,
    ) -> TurnResult:
        return collect_events(
            self.stream(
                messages=messages,
                model=model,
                user=user,
                system=system,
                extra_body=extra_body,
                include_tool_output_in_reasoning=include_tool_output_in_reasoning,
                reasoning_effort=reasoning_effort,
            )
        )


class AtomCodeClient:
    """Sync client for AtomCode ``serve`` compatible endpoints.

    Parameters
    ----------
    base_url:
        e.g. ``http://127.0.0.1:4096``
    token:
        WebUI / serve access token (``Authorization: Bearer …``).
        Omit for ``--no-token`` servers.
    timeout:
        httpx timeout (seconds or httpx.Timeout).
    """

    def __init__(
        self,
        base_url: str,
        *,
        token: Optional[str] = None,
        timeout: float | httpx.Timeout = 600.0,
        http_client: Optional[httpx.Client] = None,
    ) -> None:
        self.base_url = base_url.rstrip("/")
        self.token = token
        self._owns_client = http_client is None
        headers = {"Content-Type": "application/json"}
        if token:
            headers["Authorization"] = f"Bearer {token}"
        self._http = http_client or httpx.Client(
            base_url=self.base_url,
            headers=headers,
            timeout=timeout,
        )
        self.chat = _ChatAPI(self)
        self.responses = _ResponsesAPI(self)
        self.messages = _MessagesAPI(self)

    def close(self) -> None:
        if self._owns_client:
            self._http.close()

    def __enter__(self) -> "AtomCodeClient":
        return self

    def __exit__(self, *args: object) -> None:
        self.close()

    def list_models(self) -> dict[str, Any]:
        return self._request_json("GET", "/v1/models")

    def list_sessions(self, user: Optional[str] = None) -> dict[str, Any]:
        params = {"user": user} if user else None
        return self._request_json("GET", "/v1/sessions", params=params)

    def _request_json(
        self,
        method: str,
        path: str,
        *,
        json: Optional[dict[str, Any]] = None,
        params: Optional[dict[str, Any]] = None,
    ) -> dict[str, Any]:
        r = self._http.request(method, path, json=json, params=params)
        r.raise_for_status()
        return r.json()

    def _stream(
        self,
        method: str,
        path: str,
        *,
        json: dict[str, Any],
        protocol: Protocol,
        include_tool_output_in_reasoning: bool = False,
        reasoning_effort: ReasoningEffort | str = ReasoningEffort.MEDIUM,
    ) -> Iterator[StreamEvent]:
        parser = StreamParser(
            protocol,
            reasoning_effort=reasoning_effort,
            include_tool_output_in_reasoning=include_tool_output_in_reasoning,
        )
        with self._http.stream(method, path, json=json) as resp:
            resp.raise_for_status()
            # httpx iter_lines yields decoded strings
            for ev in parser.feed_sse_lines(resp.iter_lines()):
                yield ev


class AsyncAtomCodeClient:
    """Async variant of :class:`AtomCodeClient` (``httpx.AsyncClient``)."""

    def __init__(
        self,
        base_url: str,
        *,
        token: Optional[str] = None,
        timeout: float | httpx.Timeout = 600.0,
        http_client: Optional[httpx.AsyncClient] = None,
    ) -> None:
        self.base_url = base_url.rstrip("/")
        self.token = token
        self._owns_client = http_client is None
        headers = {"Content-Type": "application/json"}
        if token:
            headers["Authorization"] = f"Bearer {token}"
        self._http = http_client or httpx.AsyncClient(
            base_url=self.base_url,
            headers=headers,
            timeout=timeout,
        )
        self.chat = _AsyncChatAPI(self)
        self.responses = _AsyncResponsesAPI(self)
        self.messages = _AsyncMessagesAPI(self)

    async def aclose(self) -> None:
        if self._owns_client:
            await self._http.aclose()

    async def __aenter__(self) -> "AsyncAtomCodeClient":
        return self

    async def __aexit__(self, *args: object) -> None:
        await self.aclose()

    async def list_models(self) -> dict[str, Any]:
        r = await self._http.get("/v1/models")
        r.raise_for_status()
        return r.json()

    async def list_sessions(self, user: Optional[str] = None) -> dict[str, Any]:
        params = {"user": user} if user else None
        r = await self._http.get("/v1/sessions", params=params)
        r.raise_for_status()
        return r.json()

    async def _stream(
        self,
        method: str,
        path: str,
        *,
        json: dict[str, Any],
        protocol: Protocol,
        include_tool_output_in_reasoning: bool = False,
        reasoning_effort: ReasoningEffort | str = ReasoningEffort.MEDIUM,
    ) -> AsyncIterator[StreamEvent]:
        parser = StreamParser(
            protocol,
            reasoning_effort=reasoning_effort,
            include_tool_output_in_reasoning=include_tool_output_in_reasoning,
        )
        async with self._http.stream(method, path, json=json) as resp:
            resp.raise_for_status()
            async for line in resp.aiter_lines():
                for ev in parser.feed_sse_lines([line]):
                    yield ev
            # flush trailing SSE buffer (no final blank line)
            for ev in parser.feed_sse_lines([""]):
                yield ev


class _AsyncChatAPI:
    def __init__(self, client: AsyncAtomCodeClient) -> None:
        self._c = client

    async def stream(
        self,
        *,
        messages: list[Message],
        model: Optional[str] = None,
        user: Optional[str] = None,
        system: Optional[str] = None,
        extra_body: Optional[dict[str, Any]] = None,
        include_tool_output_in_reasoning: bool = False,
        reasoning_effort: ReasoningEffort | str = ReasoningEffort.MEDIUM,
    ) -> AsyncIterator[StreamEvent]:
        body = _chat_body(
            messages=messages,
            model=model,
            user=user,
            system=system,
            stream=True,
            extra_body=extra_body,
        )
        async for ev in self._c._stream(
            "POST",
            "/v1/chat/completions",
            json=body,
            protocol=Protocol.CHAT_COMPLETIONS,
            include_tool_output_in_reasoning=include_tool_output_in_reasoning,
            reasoning_effort=reasoning_effort,
        ):
            yield ev

    async def run(
        self,
        *,
        messages: list[Message],
        model: Optional[str] = None,
        user: Optional[str] = None,
        system: Optional[str] = None,
        extra_body: Optional[dict[str, Any]] = None,
        include_tool_output_in_reasoning: bool = False,
        reasoning_effort: ReasoningEffort | str = ReasoningEffort.MEDIUM,
    ) -> TurnResult:
        events: list[StreamEvent] = []
        async for ev in self.stream(
            messages=messages,
            model=model,
            user=user,
            system=system,
            extra_body=extra_body,
            include_tool_output_in_reasoning=include_tool_output_in_reasoning,
            reasoning_effort=reasoning_effort,
        ):
            events.append(ev)
        return collect_events(events)


class _AsyncResponsesAPI:
    def __init__(self, client: AsyncAtomCodeClient) -> None:
        self._c = client

    async def stream(
        self,
        *,
        input: Optional[Union[str, list[dict[str, Any]]]] = None,
        messages: Optional[list[Message]] = None,
        model: Optional[str] = None,
        user: Optional[str] = None,
        instructions: Optional[str] = None,
        extra_body: Optional[dict[str, Any]] = None,
        include_tool_output_in_reasoning: bool = False,
        reasoning_effort: ReasoningEffort | str = ReasoningEffort.MEDIUM,
    ) -> AsyncIterator[StreamEvent]:
        body: dict[str, Any] = {"stream": True}
        if input is not None:
            body["input"] = input
        if messages is not None:
            body["messages"] = messages
        if model:
            body["model"] = model
        if user:
            body["user"] = user
        if instructions:
            body["instructions"] = instructions
        if extra_body:
            body.update(extra_body)
        async for ev in self._c._stream(
            "POST",
            "/v1/responses",
            json=body,
            protocol=Protocol.RESPONSES,
            include_tool_output_in_reasoning=include_tool_output_in_reasoning,
            reasoning_effort=reasoning_effort,
        ):
            yield ev

    async def run(self, **kwargs: Any) -> TurnResult:
        events: list[StreamEvent] = []
        async for ev in self.stream(**kwargs):
            events.append(ev)
        return collect_events(events)


class _AsyncMessagesAPI:
    def __init__(self, client: AsyncAtomCodeClient) -> None:
        self._c = client

    async def stream(
        self,
        *,
        messages: list[Message],
        model: Optional[str] = None,
        user: Optional[str] = None,
        system: Optional[Union[str, list[dict[str, Any]]]] = None,
        extra_body: Optional[dict[str, Any]] = None,
        include_tool_output_in_reasoning: bool = False,
        reasoning_effort: ReasoningEffort | str = ReasoningEffort.MEDIUM,
    ) -> AsyncIterator[StreamEvent]:
        body: dict[str, Any] = {"messages": messages, "stream": True}
        if model:
            body["model"] = model
        if user:
            body["user"] = user
        if system is not None:
            body["system"] = system
        if extra_body:
            body.update(extra_body)
        async for ev in self._c._stream(
            "POST",
            "/v1/messages",
            json=body,
            protocol=Protocol.MESSAGES,
            include_tool_output_in_reasoning=include_tool_output_in_reasoning,
            reasoning_effort=reasoning_effort,
        ):
            yield ev

    async def run(self, **kwargs: Any) -> TurnResult:
        events: list[StreamEvent] = []
        async for ev in self.stream(**kwargs):
            events.append(ev)
        return collect_events(events)
