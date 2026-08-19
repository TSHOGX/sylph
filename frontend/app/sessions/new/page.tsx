"use client";

import { useMutation } from "@tanstack/react-query";
import { useRouter } from "next/navigation";
import { useState } from "react";
import { api } from "@/lib/api";

export default function NewSessionPage() {
  const router = useRouter();
  const [formData, setFormData] = useState({
    title: "",
    agent: "claude",
    workdir: "",
  });

  const createMutation = useMutation({
    mutationFn: api.sessions.create,
    onSuccess: (data) => {
      router.push(`/sessions/${data.session.id}`);
    },
  });

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    if (!formData.title || !formData.agent || !formData.workdir) {
      alert("All fields are required");
      return;
    }
    createMutation.mutate({
      ...formData,
      origin: "sylph",
    });
  };

  return (
    <div className="min-h-screen bg-background flex items-center justify-center px-4">
      <div className="w-full max-w-md">
        <h1 className="text-3xl font-bold mb-8">Create New Session</h1>

        <form onSubmit={handleSubmit} className="space-y-6">
          <div>
            <label htmlFor="title" className="block text-sm font-medium mb-2">
              Title
            </label>
            <input
              id="title"
              type="text"
              value={formData.title}
              onChange={(e) => setFormData({ ...formData, title: e.target.value })}
              className="w-full px-3 py-2 border rounded-lg focus:ring-2 focus:ring-primary focus:outline-none"
              placeholder="My project session"
            />
          </div>

          <div>
            <label htmlFor="agent" className="block text-sm font-medium mb-2">
              Agent
            </label>
            <select
              id="agent"
              value={formData.agent}
              onChange={(e) => setFormData({ ...formData, agent: e.target.value })}
              className="w-full px-3 py-2 border rounded-lg focus:ring-2 focus:ring-primary focus:outline-none"
            >
              <option value="claude">Claude</option>
              <option value="codex">Codex</option>
              <option value="pi">Pi</option>
              <option value="opencode">OpenCode</option>
            </select>
          </div>

          <div>
            <label htmlFor="workdir" className="block text-sm font-medium mb-2">
              Working Directory
            </label>
            <input
              id="workdir"
              type="text"
              value={formData.workdir}
              onChange={(e) => setFormData({ ...formData, workdir: e.target.value })}
              className="w-full px-3 py-2 border rounded-lg focus:ring-2 focus:ring-primary focus:outline-none"
              placeholder="/Users/you/project"
            />
          </div>

          {createMutation.error && (
            <div className="p-3 bg-red-50 text-red-800 rounded-lg text-sm">
              {createMutation.error instanceof Error
                ? createMutation.error.message
                : "Failed to create session"}
            </div>
          )}

          <div className="flex gap-4">
            <button
              type="button"
              onClick={() => router.push("/")}
              className="flex-1 px-4 py-2 border rounded-lg hover:bg-accent transition"
            >
              Cancel
            </button>
            <button
              type="submit"
              disabled={createMutation.isPending}
              className="flex-1 px-4 py-2 bg-primary text-primary-foreground rounded-lg hover:bg-primary/90 transition disabled:opacity-50"
            >
              {createMutation.isPending ? "Creating..." : "Create Session"}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
