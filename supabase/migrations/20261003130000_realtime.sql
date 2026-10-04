-- Habilita Realtime (WebSocket) nas tabelas de dados, para o app ser avisado
-- quando outro dispositivo alterar algo. O cliente usa o evento apenas como
-- gatilho para um novo sync (pull + merge); o conteúdo do payload não importa.
--
-- `replica identity full` faz os eventos de UPDATE/DELETE carregarem a linha
-- inteira. Sem isso o Realtime não consegue avaliar `auth.uid() = user_id` da
-- política de RLS e descarta o evento.
alter publication supabase_realtime add table public.tabs, public.todos;
alter table public.tabs replica identity full;
alter table public.todos replica identity full;
