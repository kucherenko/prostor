// Chrome-only media APIs that TypeScript's DOM library does not declare yet.
// `MediaStreamTrackProcessor` exists in Window scope only (Chrome 155), which is why the
// camera and the mixer output are read on the main thread and transferred per frame.

interface MediaStreamTrackProcessorInit {
	track: MediaStreamTrack;
	maxBufferSize?: number;
}

interface MediaStreamTrackProcessor<T extends VideoFrame | AudioData> {
	readonly readable: ReadableStream<T>;
}

declare const MediaStreamTrackProcessor: {
	prototype: MediaStreamTrackProcessor<VideoFrame | AudioData>;
	new <T extends VideoFrame | AudioData = VideoFrame>(
		init: MediaStreamTrackProcessorInit
	): MediaStreamTrackProcessor<T>;
};
